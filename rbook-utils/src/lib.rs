use anyhow::{Context, Result};
use rbook::ebook::manifest::Manifest;
use rbook::ebook::spine::Spine;
use rbook::ebook::toc::{Toc, TocChildren, TocEntry};
use rbook::{Ebook, Epub};
use rbook::prelude::{MetaEntry, Metadata, SpineEntry};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use kuchiki::traits::*;
use kuchiki::{NodeRef, parse_html};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum MarkdownMode {
    Plain,
    Rich,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum StyleMode {
    Inline,
    External,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub media_all: bool,
    pub markdown_mode: MarkdownMode,
    pub style: StyleMode,
    pub split_chapters: bool,
}

impl ConvertOptions {
    pub fn new(input_dir: PathBuf, output_dir: PathBuf) -> Self {
        Self {
            input_dir,
            output_dir,
            media_all: false,
            markdown_mode: MarkdownMode::Plain,
            style: StyleMode::Inline,
            split_chapters: false,
        }
    }
}

#[derive(Clone, Debug)]
struct TocEntryInfo {
    label: String,
    href_path: String,
    fragment: Option<String>,
}

#[derive(Clone, Debug)]
struct ContentDoc {
    href_path: String,
    document: NodeRef,
}

const COMPLEX_HTML_TAGS: &[&str] = &[
    "table",
    "thead",
    "tbody",
    "tr",
    "td",
    "th",
    "figure",
    "figcaption",
    "svg",
    "math",
];

const CONTAINER_TAGS: &[&str] = &["section", "article", "div", "body"];

const READABLE_MIME: &[&str] = &["application/xhtml+xml", "text/html"];

pub fn convert_all(options: &ConvertOptions) -> Result<()> {
    let mut epub_paths = Vec::new();
    for entry in WalkDir::new(&options.input_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if entry.file_type().is_file() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("epub") {
                epub_paths.push(path.to_path_buf());
            }
        }
    }

    if epub_paths.is_empty() {
        anyhow::bail!("No EPUB files found under {}", options.input_dir.display());
    }

    let mut failures = 0;
    for epub_path in epub_paths {
        if let Err(err) = convert_epub(&epub_path, options) {
            failures += 1;
            eprintln!("Failed to parse {}: {err}", epub_path.display());
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} EPUB(s) failed to parse");
    }

    Ok(())
}

pub fn convert_epub(epub_path: &Path, options: &ConvertOptions) -> Result<PathBuf> {
    let epub = Epub::open(epub_path).with_context(|| format!(
        "Failed to open epub {}",
        epub_path.display()
    ))?;

    let title = epub
        .metadata()
        .title()
        .map(|t| t.value().to_string())
        .unwrap_or_else(|| {
            epub_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("book")
                .to_string()
        });

    let author = epub
        .metadata()
        .creators()
        .next()
        .map(|c| c.value().to_string());

    let book_slug = slugify(&title);
    let image_root = options.output_dir.join(&book_slug).join("images");
    let style_root = options.output_dir.join(&book_slug).join("styles");

    let mut extracted_images: HashMap<String, String> = HashMap::new();
    let mut extracted_count = 0usize;

    let mut css_hrefs: HashSet<String> = HashSet::new();
    let mut inline_styles: Vec<String> = Vec::new();

    if options.media_all {
        for image in epub.manifest().images() {
            let href = image.href().as_str().to_string();
            let _ = extract_image(
                &epub,
                &href,
                &image_root,
                &book_slug,
                &mut extracted_images,
                &mut extracted_count,
            );
        }
    }

    let mut content_cache: HashMap<String, ContentDoc> = HashMap::new();

    let mut image_resolver = |src: &str, base_href: &str| -> Option<String> {
        resolve_and_extract_image(
            &epub,
            src,
            base_href,
            &image_root,
            &book_slug,
            &mut extracted_images,
            &mut extracted_count,
        )
    };

    let toc_entries = build_toc_entries(&epub)?;
    let mut sections: Vec<(String, String)> = Vec::new();

    if !toc_entries.is_empty() {
        let mut href_counts: HashMap<String, usize> = HashMap::new();
        for entry in &toc_entries {
            *href_counts.entry(entry.href_path.clone()).or_insert(0) += 1;
        }

        let mut sections_by_entry: Vec<Option<(String, String)>> = vec![None; toc_entries.len()];
        let mut href_has_section: HashMap<String, bool> = href_counts
            .keys()
            .map(|k| (k.clone(), false))
            .collect();
        let mut first_index_by_href: HashMap<String, usize> = HashMap::new();

        for (idx, entry) in toc_entries.iter().enumerate() {
            first_index_by_href.entry(entry.href_path.clone()).or_insert(idx);
            let content = match load_content(&epub, &entry.href_path, &mut content_cache) {
                Ok(content) => content,
                Err(_) => continue,
            };
            if options.markdown_mode == MarkdownMode::Rich {
                collect_css(content, &entry.href_path, &mut css_hrefs, &mut inline_styles);
            }
            let allow_body = href_counts.get(&entry.href_path).copied().unwrap_or(0) == 1;
            let text = if let Some(fragment) = &entry.fragment {
                extract_section(
                    content,
                    fragment,
                    allow_body,
                    options.markdown_mode,
                    &mut image_resolver,
                )
            } else if allow_body {
                render_full_content(
                    content,
                    options.markdown_mode,
                    &mut image_resolver,
                )
            } else {
                None
            };

            if let Some(text) = text {
                if !text.trim().is_empty() {
                    sections_by_entry[idx] = Some((entry.label.clone(), text));
                    href_has_section.insert(entry.href_path.clone(), true);
                }
            }
        }

        for (href, has_section) in href_has_section {
            if has_section {
                continue;
            }
            let first_idx = match first_index_by_href.get(&href) {
                Some(idx) => *idx,
                None => continue,
            };
            let content = match load_content(&epub, &href, &mut content_cache) {
                Ok(content) => content,
                Err(_) => continue,
            };
            if options.markdown_mode == MarkdownMode::Rich {
                collect_css(content, &href, &mut css_hrefs, &mut inline_styles);
            }
            if let Some(text) = render_full_content(
                content,
                options.markdown_mode,
                &mut image_resolver,
            ) {
                if !text.trim().is_empty() {
                    sections_by_entry[first_idx] = Some((toc_entries[first_idx].label.clone(), text));
                }
            }
        }

        sections = sections_by_entry.into_iter().flatten().collect();
    } else {
        for spine_entry in epub.spine().entries() {
            if let Some(manifest_entry) = spine_entry.manifest_entry() {
                if !is_readable(manifest_entry.media_type()) {
                    continue;
                }
                let href_path = manifest_entry.href().as_str().to_string();
                let label = manifest_entry
                    .href()
                    .name()
                    .decode()
                    .to_string();
                let content = match load_content(&epub, &href_path, &mut content_cache) {
                    Ok(content) => content,
                    Err(_) => continue,
                };
                if options.markdown_mode == MarkdownMode::Rich {
                    collect_css(content, &href_path, &mut css_hrefs, &mut inline_styles);
                }
                if let Some(text) = render_full_content(
                    content,
                    options.markdown_mode,
                    &mut image_resolver,
                ) {
                    if !text.trim().is_empty() {
                        sections.push((label, text));
                    }
                }
            }
        }
    }

    if sections.is_empty() {
        anyhow::bail!("No readable sections found in {}", epub_path.display());
    }

    let style_header_lines = if options.markdown_mode == MarkdownMode::Rich {
        build_style_header(
            &epub,
            &css_hrefs,
            &inline_styles,
            &style_root,
            &book_slug,
            options.style,
        )?
    } else {
        Vec::new()
    };

    let output_root = if options.split_chapters {
        options.output_dir.join(&book_slug)
    } else {
        options.output_dir.clone()
    };
    fs::create_dir_all(&output_root)?;

    let mut base_lines = Vec::new();
    base_lines.push(format!("# {title}"));
    if let Some(author) = author {
        base_lines.push(format!("**Author:** {author}"));
    }
    if !style_header_lines.is_empty() {
        base_lines.push(String::new());
        base_lines.extend(style_header_lines.clone());
    }
    base_lines.push(String::new());

    let mut return_path = output_root.clone();
    if options.split_chapters {
        let width = std::cmp::max(2, sections.len().to_string().len());
        for (idx, (section_title, section_text)) in sections.iter().enumerate() {
            let section_slug = if section_title.trim().is_empty() {
                format!("section_{:0width$}", idx + 1, width = width)
            } else {
                slugify(section_title)
            };
            let filename = format!(
                "{:0width$}_{}.md",
                idx + 1,
                section_slug,
                width = width
            );
            let mut lines = base_lines.clone();
            lines.push(format!("## {section_title}"));
            lines.push(String::new());
            lines.push(section_text.clone());
            lines.push(String::new());
            fs::write(output_root.join(filename), lines.join("\n").trim().to_string() + "\n")?;
        }
    } else {
        let output_path = output_root.join(format!("{book_slug}.md"));
        let mut lines = base_lines;
        for (section_title, section_text) in sections {
            lines.push(format!("## {section_title}"));
            lines.push(String::new());
            lines.push(section_text);
            lines.push(String::new());
        }
        fs::write(&output_path, lines.join("\n").trim().to_string() + "\n")?;
        return_path = output_path;
    }

    if extracted_count > 0 {
        println!("Extracted {extracted_count} images for {title}");
    }

    Ok(return_path)
}

fn build_toc_entries(epub: &Epub) -> Result<Vec<TocEntryInfo>> {
    let mut entries = Vec::new();
    if let Some(root) = epub.toc().contents() {
        for entry in root.children().flatten() {
            let href = match entry.href() {
                Some(href) => href,
                None => continue,
            };
            if let Some(manifest_entry) = entry.manifest_entry() {
                if !is_readable(manifest_entry.media_type()) {
                    continue;
                }
            }
            let label = entry.label().to_string();
            let href_path = href.path().as_str().to_string();
            let fragment = href.fragment().map(|frag| frag.to_string());
            entries.push(TocEntryInfo {
                label,
                href_path,
                fragment,
            });
        }
    }
    Ok(entries)
}

fn load_content<'a>(
    epub: &Epub,
    href_path: &str,
    cache: &'a mut HashMap<String, ContentDoc>,
) -> Result<&'a ContentDoc> {
    if !cache.contains_key(href_path) {
        let html = epub
            .read_resource_str(href_path)
            .with_context(|| format!("Failed to read {href_path}"))?;
        let document = parse_html().one(html);
        cache.insert(
            href_path.to_string(),
            ContentDoc {
                href_path: href_path.to_string(),
                document,
            },
        );
    }
    Ok(cache.get(href_path).expect("cache insert"))
}

fn is_readable(media_type: &str) -> bool {
    READABLE_MIME.iter().any(|mime| mime.eq_ignore_ascii_case(media_type))
}

fn collect_css(content: &ContentDoc, base_href: &str, css_hrefs: &mut HashSet<String>, inline_styles: &mut Vec<String>) {
    if let Ok(head) = content.document.select_first("head") {
        let node = head.as_node();
        if let Ok(links) = node.select("link[rel~='stylesheet']") {
            for link in links {
                let attrs = link.attributes.borrow();
                if let Some(href) = attrs.get("href") {
                    if is_external(href) {
                        continue;
                    }
                    let resolved = resolve_href(base_href, href);
                    css_hrefs.insert(resolved);
                }
            }
        }
        if let Ok(styles) = node.select("style") {
            for style_node in styles {
                let text = style_node.text_contents();
                if !text.trim().is_empty() {
                    inline_styles.push(text);
                }
            }
        }
    }
}

fn build_style_header(
    epub: &Epub,
    css_hrefs: &HashSet<String>,
    inline_styles: &[String],
    styles_root: &Path,
    book_slug: &str,
    style_mode: StyleMode,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    if css_hrefs.is_empty() && inline_styles.is_empty() {
        return Ok(lines);
    }

    match style_mode {
        StyleMode::External => {
            for href in css_hrefs.iter().collect::<Vec<_>>() {
                let bytes = epub.read_resource_bytes(href.as_str())?;
                let relative = decode_path(href);
                let output_path = styles_root.join(&relative);
                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&output_path, bytes)?;
                lines.push(format!(
                    "<link rel=\"stylesheet\" href=\"./{book_slug}/styles/{relative}\">"
                ));
            }

            if !inline_styles.is_empty() {
                fs::create_dir_all(styles_root)?;
                let inline_path = styles_root.join("inline_styles.css");
                fs::write(&inline_path, inline_styles.join("\n\n"))?;
                lines.push(format!(
                    "<link rel=\"stylesheet\" href=\"./{book_slug}/styles/inline_styles.css\">"
                ));
            }
        }
        StyleMode::Inline => {
            let mut css_chunks = Vec::new();
            for href in css_hrefs.iter().collect::<Vec<_>>() {
                let bytes = epub.read_resource_bytes(href.as_str())?;
                let css = String::from_utf8_lossy(&bytes).to_string();
                css_chunks.push(css);
            }
            css_chunks.extend(inline_styles.iter().cloned());
            if !css_chunks.is_empty() {
                lines.push("<style>".to_string());
                lines.push(css_chunks.join("\n\n"));
                lines.push("</style>".to_string());
            }
        }
    }

    Ok(lines)
}

fn extract_section(
    content: &ContentDoc,
    fragment: &str,
    allow_body: bool,
    markdown_mode: MarkdownMode,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    let anchor = find_anchor(&content.document, fragment)?;
    let container = find_container(&anchor, allow_body).unwrap_or(anchor.clone());
    match markdown_mode {
        MarkdownMode::Plain => render_plain(&container, content, image_resolver),
        MarkdownMode::Rich => Some(render_rich(&container, content, image_resolver)),
    }
}

fn render_full_content(
    content: &ContentDoc,
    markdown_mode: MarkdownMode,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    if let Ok(body) = content.document.select_first("body") {
        let body = body.as_node().clone();
        match markdown_mode {
            MarkdownMode::Plain => render_plain(&body, content, image_resolver),
            MarkdownMode::Rich => Some(render_rich(&body, content, image_resolver)),
        }
    } else {
        None
    }
}

fn render_plain(
    node: &NodeRef,
    content: &ContentDoc,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    rewrite_images(node, content, image_resolver);
    let html = serialize_children(node);
    let md = html2md::parse_html(&html);
    let trimmed = md.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

fn render_rich(
    node: &NodeRef,
    content: &ContentDoc,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> String {
    let mut chunks = Vec::new();
    for child in node.children() {
        if let Some(text) = child.as_text() {
            let t = text.borrow();
            if !t.trim().is_empty() {
                chunks.push(t.trim().to_string());
            }
            continue;
        }
        if is_complex(&child) {
            rewrite_images(&child, content, image_resolver);
            chunks.push(serialize_node(&child));
        } else {
            rewrite_images(&child, content, image_resolver);
            let html = serialize_node(&child);
            let md = html2md::parse_html(&html);
            if !md.trim().is_empty() {
                chunks.push(md.trim().to_string());
            }
        }
    }
    chunks.join("\n\n")
}

fn rewrite_images(
    node: &NodeRef,
    content: &ContentDoc,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) {
    if let Ok(images) = node.select("img") {
        for img in images {
            let mut attrs = img.attributes.borrow_mut();
            if let Some(src) = attrs.get("src") {
                if let Some(resolved) = image_resolver(src, &content.href_path) {
                    attrs.insert("src", resolved);
                }
            }
        }
    }
}

fn find_anchor(document: &NodeRef, fragment: &str) -> Option<NodeRef> {
    if let Ok(nodes) = document.select("[id]") {
        for node in nodes {
            let attrs = node.attributes.borrow();
            if let Some(id) = attrs.get("id") {
                if id == fragment {
                    return Some(node.as_node().clone());
                }
            }
        }
    }
    if let Ok(nodes) = document.select("a[name]") {
        for node in nodes {
            let attrs = node.attributes.borrow();
            if let Some(name) = attrs.get("name") {
                if name == fragment {
                    return Some(node.as_node().clone());
                }
            }
        }
    }
    None
}

fn find_container(anchor: &NodeRef, allow_body: bool) -> Option<NodeRef> {
    let mut current = anchor.clone();
    loop {
        if let Some(tag) = element_name(&current) {
            if CONTAINER_TAGS.contains(&tag) {
                if tag == "body" && !allow_body {
                    return None;
                }
                return Some(current.clone());
            }
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    None
}

fn element_name(node: &NodeRef) -> Option<&str> {
    node.as_element().map(|el| el.name.local.as_ref())
}

fn is_complex(node: &NodeRef) -> bool {
    if let Some(tag) = element_name(node) {
        if COMPLEX_HTML_TAGS.contains(&tag) {
            return true;
        }
    }
    if let Some(el) = node.as_element() {
        let attrs = el.attributes.borrow();
        if attrs.get("class").is_some() || attrs.get("style").is_some() {
            return true;
        }
    }
    for descendant in node.descendants() {
        if let Some(el) = descendant.as_element() {
            let attrs = el.attributes.borrow();
            if attrs.get("class").is_some() || attrs.get("style").is_some() {
                return true;
            }
        }
    }
    false
}

fn serialize_node(node: &NodeRef) -> String {
    let mut bytes = Vec::new();
    node.serialize(&mut bytes).ok();
    String::from_utf8_lossy(&bytes).to_string()
}

fn serialize_children(node: &NodeRef) -> String {
    let mut out = String::new();
    for child in node.children() {
        out.push_str(&serialize_node(&child));
    }
    out
}

fn resolve_and_extract_image(
    epub: &Epub,
    src: &str,
    base_href: &str,
    image_root: &Path,
    book_slug: &str,
    extracted: &mut HashMap<String, String>,
    extracted_count: &mut usize,
) -> Option<String> {
    if src.trim().is_empty() || is_external(src) {
        return Some(src.to_string());
    }
    let resolved = resolve_href(base_href, src);
    if let Some(existing) = extracted.get(&resolved) {
        return Some(existing.clone());
    }

    let bytes = match epub.read_resource_bytes(resolved.as_str()) {
        Ok(bytes) => bytes,
        Err(_) => return Some(src.to_string()),
    };

    let relative = decode_path(&resolved);
    let output_path = image_root.join(&relative);
    if let Some(parent) = output_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(&output_path, bytes).is_ok() {
        *extracted_count += 1;
        let rel_path = format!("./{book_slug}/images/{relative}");
        extracted.insert(resolved.clone(), rel_path.clone());
        Some(rel_path)
    } else {
        Some(src.to_string())
    }
}

fn extract_image(
    epub: &Epub,
    resolved: &str,
    image_root: &Path,
    book_slug: &str,
    extracted: &mut HashMap<String, String>,
    extracted_count: &mut usize,
) -> Option<String> {
    if let Some(existing) = extracted.get(resolved) {
        return Some(existing.clone());
    }
    let bytes = epub.read_resource_bytes(resolved).ok()?;
    let relative = decode_path(resolved);
    let output_path = image_root.join(&relative);
    if let Some(parent) = output_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&output_path, bytes).ok()?;
    *extracted_count += 1;
    let rel_path = format!("./{book_slug}/images/{relative}");
    extracted.insert(resolved.to_string(), rel_path.clone());
    Some(rel_path)
}

fn resolve_href(base_href: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        normalize_path(rel)
    } else {
        let base_dir = base_href.rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or("");
        let combined = format!("{base_dir}/{rel}");
        normalize_path(&combined)
    }
}

fn normalize_path(path: &str) -> String {
    let mut parts = Vec::new();
    let absolute = path.starts_with('/');
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

fn decode_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    urlencoding::decode(trimmed)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn is_external(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("data:")
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
            out.push(ch);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = out.trim_matches(&['_', '.', '-'][..]).to_string();
    if trimmed.is_empty() {
        "book".to_string()
    } else {
        trimmed
    }
}
