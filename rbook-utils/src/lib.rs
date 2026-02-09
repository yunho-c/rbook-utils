use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ChapterFallbackMode {
    Off,
    Auto,
    Force,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub media_all: bool,
    pub markdown_mode: MarkdownMode,
    pub style: StyleMode,
    pub split_chapters: bool,
    pub chapter_fallback: ChapterFallbackMode,
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
            chapter_fallback: ChapterFallbackMode::Auto,
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

#[derive(Clone, Debug)]
struct HeadingCandidate {
    spine_idx: usize,
    score: f32,
    label: String,
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

const READABLE_MIME: &[&str] = &["application/xhtml+xml", "text/html"];
static MAJOR_HEADING_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(?:chapter|book|part)\s+(?:[ivxlcdm]+|\d+)\b|\b(?:preface|prologue|epilogue|introduction|foreword|afterword)\b",
    )
    .expect("valid heading regex")
});
static MAJOR_HEADING_LABEL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(?:chapter|book|part)\s+(?:[ivxlcdm]+|\d+)(?:\s*[:.-]?\s*[a-z0-9][a-z0-9' -]{0,70})?|\b(?:preface|prologue|epilogue|introduction|foreword|afterword)\b",
    )
    .expect("valid heading label regex")
});
static OCR_NOISE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)estimated\s+to\s+be\s+only\s+\d+(?:\.\d+)?%\s+accurate")
        .expect("valid ocr regex")
});

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
    let image_link_prefix = if options.split_chapters {
        "./images".to_string()
    } else {
        format!("./{book_slug}/images")
    };
    let style_link_prefix = if options.split_chapters {
        "./styles".to_string()
    } else {
        format!("./{book_slug}/styles")
    };

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
                &image_link_prefix,
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
            &image_link_prefix,
            &mut extracted_images,
            &mut extracted_count,
        )
    };

    let toc_entries = build_toc_entries(&epub)?;
    let spine_hrefs: Vec<String> = epub
        .spine()
        .entries()
        .filter_map(|entry| entry.manifest_entry())
        .filter(|entry| is_readable(entry.media_type()))
        .map(|entry| entry.href().as_str().to_string())
        .collect();
    let spine_index_by_href: HashMap<String, usize> = spine_hrefs
        .iter()
        .enumerate()
        .map(|(idx, href)| (href.clone(), idx))
        .collect();
    let (toc_is_degenerate, toc_entry_count, toc_unique_count, toc_coverage_ratio) =
        toc_degeneracy_stats(&toc_entries, spine_hrefs.len());
    let mut sections: Vec<(String, String)> = Vec::new();

    let mut use_heading_fallback = false;
    let attempt_heading_fallback = match options.chapter_fallback {
        ChapterFallbackMode::Off => false,
        ChapterFallbackMode::Auto => {
            if toc_is_degenerate {
                true
            } else {
                eprintln!(
                    "Warning: heading fallback skipped for {}: TOC not degenerate (entries={}, unique_hrefs={}, coverage={:.2}).",
                    title, toc_entry_count, toc_unique_count, toc_coverage_ratio
                );
                false
            }
        }
        ChapterFallbackMode::Force => true,
    };

    if attempt_heading_fallback {
        let heading_candidates = detect_heading_candidates(&spine_hrefs, &mut content_cache, &epub);
        let confident_candidates: Vec<HeadingCandidate> = heading_candidates
            .into_iter()
            .filter(|candidate| candidate.spine_idx > 0)
            .collect();
        if !confident_candidates.is_empty() {
            let first_label = toc_entries
                .first()
                .map(|entry| entry.label.clone())
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| {
                    spine_hrefs
                        .first()
                        .map(|href| prettify_section_name(href))
                        .unwrap_or_else(|| "Section 1".to_string())
                });
            let mut starts: Vec<(usize, String)> = vec![(0, first_label)];
            for candidate in &confident_candidates {
                let label = if candidate.label.trim().is_empty() {
                    format!("Section {}", starts.len() + 1)
                } else {
                    candidate.label.clone()
                };
                starts.push((candidate.spine_idx, label));
            }

            eprintln!(
                "Warning: using heading fallback for {} (mode={:?}, toc_entries={}, spine_docs={}, detected_starts={}).",
                title,
                options.chapter_fallback,
                toc_entry_count,
                spine_hrefs.len(),
                confident_candidates.len()
            );
            use_heading_fallback = true;

            for (start_pos, (start_idx, section_label)) in starts.iter().enumerate() {
                let next_start = starts
                    .get(start_pos + 1)
                    .map(|(idx, _)| *idx)
                    .unwrap_or(spine_hrefs.len());
                if next_start == 0 || next_start <= *start_idx {
                    continue;
                }
                let end_idx = next_start - 1;
                let mut chunks: Vec<String> = Vec::new();
                for spine_idx in *start_idx..=end_idx {
                    let Some(href) = spine_hrefs.get(spine_idx) else {
                        continue;
                    };
                    let content = match load_content(&epub, href, &mut content_cache) {
                        Ok(content) => content,
                        Err(_) => continue,
                    };
                    if options.markdown_mode == MarkdownMode::Rich {
                        collect_css(content, href, &mut css_hrefs, &mut inline_styles);
                    }
                    if let Some(part) = render_full_content(
                        content,
                        options.markdown_mode,
                        &mut image_resolver,
                    ) {
                        if !part.trim().is_empty() {
                            chunks.push(part);
                        }
                    }
                }
                let text = chunks.join("\n\n").trim().to_string();
                if !text.is_empty() {
                    sections.push((section_label.clone(), text));
                }
            }
        } else {
            eprintln!(
                "Warning: heading fallback skipped for {}: insufficient heading confidence.",
                title
            );
        }
    }

    if !use_heading_fallback && !toc_entries.is_empty() {
        for (idx, entry) in toc_entries.iter().enumerate() {
            let Some(start_idx) = spine_index_by_href.get(&entry.href_path).copied() else {
                continue;
            };
            let next_entry = toc_entries.get(idx + 1);
            let end_idx = if let Some(next) = next_entry {
                spine_index_by_href
                    .get(&next.href_path)
                    .copied()
                    .unwrap_or(spine_hrefs.len().saturating_sub(1))
            } else {
                spine_hrefs.len().saturating_sub(1)
            };
            if end_idx < start_idx {
                continue;
            }

            let mut chunks: Vec<String> = Vec::new();
            for spine_idx in start_idx..=end_idx {
                let Some(href) = spine_hrefs.get(spine_idx) else {
                    continue;
                };
                let content = match load_content(&epub, href, &mut content_cache) {
                    Ok(content) => content,
                    Err(_) => continue,
                };
                if options.markdown_mode == MarkdownMode::Rich {
                    collect_css(content, href, &mut css_hrefs, &mut inline_styles);
                }

                if let Some(next) = next_entry {
                    if spine_idx == end_idx && next.fragment.is_none() {
                        // Next section starts at the beginning of this file.
                        continue;
                    }
                }

                let start_fragment = if spine_idx == start_idx {
                    entry.fragment.as_deref()
                } else {
                    None
                };
                let end_fragment = if let Some(next) = next_entry {
                    if spine_idx == end_idx {
                        next.fragment.as_deref()
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(part) = render_partial_content(
                    content,
                    options.markdown_mode,
                    start_fragment,
                    end_fragment,
                    &mut image_resolver,
                ) {
                    if !part.trim().is_empty() {
                        chunks.push(part);
                    }
                }
            }

            let text = chunks.join("\n\n").trim().to_string();
            if !text.is_empty() {
                sections.push((entry.label.clone(), text));
            }
        }
    } else if !use_heading_fallback {
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
            &style_link_prefix,
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
        if output_root.exists() {
            for entry in fs::read_dir(&output_root)? {
                let path = entry?.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                    let _ = fs::remove_file(path);
                }
            }
        }
        let width = std::cmp::max(2, sections.len().to_string().len());
        for (idx, (section_title, section_text)) in sections.iter().enumerate() {
            let mut section_slug = if section_title.trim().is_empty() {
                format!("section_{:0width$}", idx + 1, width = width)
            } else {
                slugify(section_title)
            };
            section_slug = section_slug
                .chars()
                .take(80)
                .collect::<String>()
                .trim_matches(&['_', '.', '-'][..])
                .to_string();
            if section_slug.is_empty() {
                section_slug = format!("section_{:0width$}", idx + 1, width = width);
            }
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

fn toc_degeneracy_stats(
    toc_entries: &[TocEntryInfo],
    spine_doc_count: usize,
) -> (bool, usize, usize, f32) {
    let toc_entry_count = toc_entries.len();
    let unique_toc_hrefs: HashSet<&str> =
        toc_entries.iter().map(|entry| entry.href_path.as_str()).collect();
    let unique_count = unique_toc_hrefs.len();
    let coverage_ratio = if spine_doc_count > 0 {
        unique_count as f32 / spine_doc_count as f32
    } else {
        0.0
    };
    let is_degenerate =
        toc_entry_count <= 1 || unique_count < 3 || coverage_ratio < 0.15;
    (is_degenerate, toc_entry_count, unique_count, coverage_ratio)
}

fn detect_heading_candidates(
    spine_hrefs: &[String],
    cache: &mut HashMap<String, ContentDoc>,
    epub: &Epub,
) -> Vec<HeadingCandidate> {
    let mut accepted: Vec<HeadingCandidate> = Vec::new();
    let min_gap_docs = 2usize;

    for (idx, href) in spine_hrefs.iter().enumerate() {
        let content = match load_content(epub, href, cache) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let (score, label, true_heading) = score_heading_candidate(content);
        if score < 1.0 {
            continue;
        }
        if idx == 0 && !true_heading {
            continue;
        }

        let candidate = HeadingCandidate {
            spine_idx: idx,
            score,
            label: clean_heading_label(&label),
        };

        if let Some(prev) = accepted.last_mut() {
            if idx.saturating_sub(prev.spine_idx) < min_gap_docs {
                if candidate.score > prev.score {
                    *prev = candidate;
                }
                continue;
            }
        }
        accepted.push(candidate);
    }

    accepted
}

fn score_heading_candidate(content: &ContentDoc) -> (f32, String, bool) {
    let (top_window_text, first_nonempty_line, heading_texts) =
        extract_heading_features(content);

    let mut score = 0.0f32;
    let mut label = String::new();
    let mut heading_match = false;

    for heading_text in &heading_texts {
        if MAJOR_HEADING_RE.is_match(heading_text) {
            score += 0.9;
            heading_match = true;
            label = extract_major_heading_label(heading_text)
                .unwrap_or_else(|| clean_heading_label(heading_text));
            break;
        }
    }

    let top_match = MAJOR_HEADING_RE.find(&top_window_text);
    if top_match.is_some() {
        score += 0.8;
        if label.is_empty() {
            if !first_nonempty_line.is_empty() && MAJOR_HEADING_RE.is_match(&first_nonempty_line)
            {
                label = extract_major_heading_label(&first_nonempty_line)
                    .unwrap_or_else(|| clean_heading_label(&first_nonempty_line));
            } else if let Some(found) = top_match {
                label = extract_major_heading_label(&top_window_text)
                    .unwrap_or_else(|| clean_heading_label(found.as_str()));
            }
        }
    }

    let first_line_major_match =
        !first_nonempty_line.is_empty() && MAJOR_HEADING_RE.is_match(&first_nonempty_line);
    if !first_nonempty_line.is_empty()
        && (is_heading_like_line(&first_nonempty_line) || first_line_major_match)
    {
        score += 0.4;
        if label.is_empty() && first_line_major_match {
            label = extract_major_heading_label(&first_nonempty_line)
                .unwrap_or_else(|| clean_heading_label(&first_nonempty_line));
        }
    }

    if OCR_NOISE_RE.is_match(&top_window_text) {
        score -= 0.5;
    }

    score = score.clamp(0.0, 2.0);
    let true_heading = heading_match || top_match.is_some();
    (score, label, true_heading)
}

fn extract_heading_features(content: &ContentDoc) -> (String, String, Vec<String>) {
    let Ok(body) = content.document.select_first("body") else {
        return (String::new(), String::new(), Vec::new());
    };
    let body_node = body.as_node();
    let body_text = body_node.text_contents();
    let top_window_raw: String = body_text.chars().take(1500).collect();
    let top_window_text = normalize_space(&top_window_raw);

    let mut first_nonempty_line = String::new();
    for line in top_window_raw.lines() {
        let stripped = normalize_space(line);
        if !stripped.is_empty() {
            first_nonempty_line = stripped;
            break;
        }
    }
    if first_nonempty_line.is_empty() && !top_window_text.is_empty() {
        first_nonempty_line = top_window_text.chars().take(80).collect::<String>();
    }

    let mut heading_texts: Vec<String> = Vec::new();
    if let Ok(headings) = body_node.select("h1, h2, h3") {
        for heading in headings {
            let text = normalize_space(&heading.text_contents());
            if !text.is_empty() {
                heading_texts.push(text);
            }
        }
    }

    (top_window_text, first_nonempty_line, heading_texts)
}

fn is_heading_like_line(line: &str) -> bool {
    let normalized = normalize_space(line);
    if normalized.is_empty() || normalized.chars().count() > 80 {
        return false;
    }
    let words: Vec<&str> = normalized
        .split_whitespace()
        .filter(|word| word.chars().any(|c| c.is_alphabetic()))
        .collect();
    if words.is_empty() {
        return false;
    }
    let letters: Vec<char> = normalized.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return false;
    }
    let all_caps = letters.iter().all(|c| !c.is_lowercase());
    let title_like = words
        .iter()
        .filter(|word| word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
        .count()
        >= std::cmp::max(1, (words.len() * 8) / 10);
    all_caps || title_like
}

fn normalize_space(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_heading_label(text: &str) -> String {
    let normalized = normalize_space(text);
    normalized
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .to_string()
}

fn extract_major_heading_label(text: &str) -> Option<String> {
    MAJOR_HEADING_LABEL_RE
        .find(text)
        .map(|m| clean_heading_label(m.as_str()))
        .filter(|label| !label.is_empty())
}

fn prettify_section_name(value: &str) -> String {
    let file_name = value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(value);
    let cleaned = file_name.replace(['_', '-'], " ");
    let cleaned = normalize_space(&cleaned);
    if cleaned.is_empty() {
        value.to_string()
    } else {
        cleaned
    }
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
    style_link_prefix: &str,
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
                    "<link rel=\"stylesheet\" href=\"{style_link_prefix}/{relative}\">"
                ));
            }

            if !inline_styles.is_empty() {
                fs::create_dir_all(styles_root)?;
                let inline_path = styles_root.join("inline_styles.css");
                fs::write(&inline_path, inline_styles.join("\n\n"))?;
                lines.push(format!(
                    "<link rel=\"stylesheet\" href=\"{style_link_prefix}/inline_styles.css\">"
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

fn render_partial_content(
    content: &ContentDoc,
    markdown_mode: MarkdownMode,
    start_fragment: Option<&str>,
    end_fragment: Option<&str>,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    if start_fragment.is_none() && end_fragment.is_none() {
        return render_full_content(content, markdown_mode, image_resolver);
    }

    let body = content.document.select_first("body").ok()?.as_node().clone();
    let children: Vec<NodeRef> = body.children().collect();
    if children.is_empty() {
        return None;
    }

    let mut start_idx = 0usize;
    if let Some(fragment) = start_fragment {
        let anchor = find_anchor(&content.document, fragment)?;
        let top = top_level_body_child(&body, &anchor)?;
        start_idx = child_index(&children, &top)?;
    }

    let mut end_idx = children.len();
    if let Some(fragment) = end_fragment {
        if let Some(anchor) = find_anchor(&content.document, fragment) {
            if let Some(top) = top_level_body_child(&body, &anchor) {
                if let Some(idx) = child_index(&children, &top) {
                    if idx > start_idx {
                        end_idx = idx;
                    }
                }
            }
        }
    }

    if start_idx >= end_idx {
        return None;
    }
    let nodes = &children[start_idx..end_idx];
    render_nodes_for_mode(nodes, content, markdown_mode, image_resolver)
}

fn render_nodes_for_mode(
    nodes: &[NodeRef],
    content: &ContentDoc,
    markdown_mode: MarkdownMode,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    match markdown_mode {
        MarkdownMode::Plain => render_nodes_plain(nodes, content, image_resolver),
        MarkdownMode::Rich => {
            let rich = render_nodes_rich(nodes, content, image_resolver);
            if rich.trim().is_empty() {
                None
            } else {
                Some(rich.trim().to_string())
            }
        }
    }
}

fn render_nodes_plain(
    nodes: &[NodeRef],
    content: &ContentDoc,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    let mut html = String::new();
    for node in nodes {
        rewrite_images(node, content, image_resolver);
        html.push_str(&serialize_node(node));
    }
    let md = html2md::parse_html(&html);
    let trimmed = md.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn render_nodes_rich(
    nodes: &[NodeRef],
    content: &ContentDoc,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> String {
    let mut chunks = Vec::new();
    for node in nodes {
        if let Some(text) = node.as_text() {
            let t = text.borrow();
            if !t.trim().is_empty() {
                chunks.push(t.trim().to_string());
            }
            continue;
        }
        if is_complex(node) {
            rewrite_images(node, content, image_resolver);
            chunks.push(serialize_node(node));
        } else {
            rewrite_images(node, content, image_resolver);
            let html = serialize_node(node);
            let md = html2md::parse_html(&html);
            if !md.trim().is_empty() {
                chunks.push(md.trim().to_string());
            }
        }
    }
    chunks.join("\n\n")
}

fn top_level_body_child(body: &NodeRef, node: &NodeRef) -> Option<NodeRef> {
    let mut current = node.clone();
    loop {
        let parent = current.parent()?;
        if parent == *body {
            return Some(current);
        }
        current = parent;
    }
}

fn child_index(children: &[NodeRef], target: &NodeRef) -> Option<usize> {
    children.iter().position(|child| child == target)
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
    image_link_prefix: &str,
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
        let rel_path = format!("{image_link_prefix}/{relative}");
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
    image_link_prefix: &str,
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
    let rel_path = format!("{image_link_prefix}/{relative}");
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
