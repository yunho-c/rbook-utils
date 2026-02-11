use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use rbook::ebook::manifest::Manifest;
use rbook::ebook::spine::Spine;
use rbook::ebook::toc::{Toc, TocChildren, TocEntry};
use rbook::prelude::{ManifestEntry, MetaEntry, Metadata, SpineEntry};
use rbook::{Ebook, Epub};
use regex::Regex;
use serde_json::json;
use sha1::{Digest, Sha1};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum NotesMode {
    Inline,
    ChapterEnd,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ExportMode {
    Off,
    V1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OcrCleanupMode {
    Off,
    Basic,
    Aggressive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum NavCleanupMode {
    Off,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FilenameScheme {
    Legacy,
    Stable,
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
    pub notes_mode: NotesMode,
    pub export_manifest: ExportMode,
    pub quality_report: ExportMode,
    pub ocr_cleanup: OcrCleanupMode,
    pub nav_cleanup: NavCleanupMode,
    pub filename_scheme: FilenameScheme,
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
            notes_mode: NotesMode::Inline,
            export_manifest: ExportMode::Off,
            quality_report: ExportMode::Off,
            ocr_cleanup: OcrCleanupMode::Off,
            nav_cleanup: NavCleanupMode::Auto,
            filename_scheme: FilenameScheme::Legacy,
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

#[derive(Clone, Debug)]
struct SectionRecord {
    title: String,
    text: String,
    start_href: String,
    start_fragment: Option<String>,
    end_href: Option<String>,
    end_fragment: Option<String>,
    spine_start: usize,
    spine_end: usize,
    anchors: Vec<String>,
    section_id: String,
    output_path: String,
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
static MARKDOWN_LINK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(!?)\[([^\]]+)\]\(([^)]+)\)").expect("valid markdown link regex"));
static HTML_HREF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(<a\b[^>]*?\bhref=")([^"]+)(")"#).expect("valid html href regex")
});
static FOOTNOTE_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\[\^([^\]]+)\]:\s*(.*)$").expect("valid footnote regex"));

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
    let epub = Epub::open(epub_path)
        .with_context(|| format!("Failed to open epub {}", epub_path.display()))?;

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
    let book_dir = options.output_dir.join(&book_slug);
    let image_root = book_dir.join("images");
    let media_root = book_dir.join("media");
    let style_root = book_dir.join("styles");
    let image_link_prefix = if options.split_chapters {
        "./images".to_string()
    } else {
        format!("./{book_slug}/images")
    };
    let media_link_prefix = if options.split_chapters {
        "./media".to_string()
    } else {
        format!("./{book_slug}/media")
    };
    let style_link_prefix = if options.split_chapters {
        "./styles".to_string()
    } else {
        format!("./{book_slug}/styles")
    };

    let mut extracted_images: HashMap<String, String> = HashMap::new();
    let mut extracted_media: HashMap<String, String> = HashMap::new();
    let mut extracted_count = 0usize;
    let mut extracted_media_count = 0usize;

    let mut css_hrefs: HashSet<String> = HashSet::new();
    let mut inline_styles: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let mut warn = |message: String| {
        eprintln!("Warning: {message}");
        warnings.push(format!("Warning: {message}"));
    };

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
        for entry in epub.manifest().entries() {
            let kind = entry.resource_kind();
            if !(kind.is_audio() || kind.is_video()) {
                continue;
            }
            let href = entry.href().as_str().to_string();
            let _ = extract_media_file(
                &epub,
                &href,
                &media_root,
                &media_link_prefix,
                &mut extracted_media,
                &mut extracted_media_count,
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

    let toc_entries_raw = build_toc_entries(&epub)?;
    let (toc_entries, nav_removed) = cleanup_toc_entries(toc_entries_raw, options.nav_cleanup);
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
    let mut sections: Vec<SectionRecord> = Vec::new();

    let mut use_heading_fallback = false;
    let attempt_heading_fallback = match options.chapter_fallback {
        ChapterFallbackMode::Off => false,
        ChapterFallbackMode::Auto => {
            if toc_is_degenerate {
                true
            } else {
                warn(format!(
                    "heading fallback skipped for {}: TOC not degenerate (entries={}, unique_hrefs={}, coverage={:.2}).",
                    title, toc_entry_count, toc_unique_count, toc_coverage_ratio
                ));
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

            warn(format!(
                "using heading fallback for {} (mode={:?}, toc_entries={}, spine_docs={}, detected_starts={}).",
                title,
                options.chapter_fallback,
                toc_entry_count,
                spine_hrefs.len(),
                confident_candidates.len()
            ));
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
                let mut anchors: HashSet<String> = HashSet::new();
                for spine_idx in *start_idx..=end_idx {
                    let Some(href) = spine_hrefs.get(spine_idx) else {
                        continue;
                    };
                    let content = match load_content(&epub, href, &mut content_cache) {
                        Ok(content) => content,
                        Err(err) => {
                            errors.push(err.to_string());
                            continue;
                        }
                    };
                    if options.markdown_mode == MarkdownMode::Rich {
                        collect_css(content, href, &mut css_hrefs, &mut inline_styles);
                    }
                    let (part, part_anchors) = render_partial_with_anchors(
                        content,
                        options.markdown_mode,
                        None,
                        None,
                        &mut image_resolver,
                    );
                    for anchor in part_anchors {
                        anchors.insert(anchor);
                    }
                    if let Some(part) = part {
                        if !part.trim().is_empty() {
                            chunks.push(part);
                        }
                    }
                }
                let text = chunks.join("\n\n").trim().to_string();
                if !text.is_empty() {
                    sections.push(SectionRecord {
                        title: section_label.clone(),
                        text,
                        start_href: spine_hrefs[*start_idx].clone(),
                        start_fragment: None,
                        end_href: Some(spine_hrefs[end_idx].clone()),
                        end_fragment: None,
                        spine_start: *start_idx,
                        spine_end: end_idx,
                        anchors: {
                            let mut values: Vec<String> = anchors.into_iter().collect();
                            values.sort();
                            values
                        },
                        section_id: String::new(),
                        output_path: String::new(),
                    });
                }
            }
        } else {
            warn(format!(
                "heading fallback skipped for {}: insufficient heading confidence.",
                title
            ));
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
            let mut section_anchors: HashSet<String> = HashSet::new();
            for spine_idx in start_idx..=end_idx {
                let Some(href) = spine_hrefs.get(spine_idx) else {
                    continue;
                };
                let content = match load_content(&epub, href, &mut content_cache) {
                    Ok(content) => content,
                    Err(err) => {
                        errors.push(err.to_string());
                        continue;
                    }
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

                let (part, part_anchors) = render_partial_with_anchors(
                    content,
                    options.markdown_mode,
                    start_fragment,
                    end_fragment,
                    &mut image_resolver,
                );
                for anchor in part_anchors {
                    section_anchors.insert(anchor);
                }
                if let Some(part) = part {
                    if !part.trim().is_empty() {
                        chunks.push(part);
                    }
                }
            }

            let text = chunks.join("\n\n").trim().to_string();
            if !text.is_empty() {
                sections.push(SectionRecord {
                    title: entry.label.clone(),
                    text,
                    start_href: entry.href_path.clone(),
                    start_fragment: entry.fragment.clone(),
                    end_href: next_entry.map(|n| n.href_path.clone()),
                    end_fragment: next_entry.and_then(|n| n.fragment.clone()),
                    spine_start: start_idx,
                    spine_end: end_idx,
                    anchors: {
                        let mut values: Vec<String> = section_anchors.into_iter().collect();
                        values.sort();
                        values
                    },
                    section_id: String::new(),
                    output_path: String::new(),
                });
            }
        }
    } else if !use_heading_fallback {
        for spine_entry in epub.spine().entries() {
            if let Some(manifest_entry) = spine_entry.manifest_entry() {
                if !is_readable(manifest_entry.media_type()) {
                    continue;
                }
                let href_path = manifest_entry.href().as_str().to_string();
                let label = manifest_entry.href().name().decode().to_string();
                let content = match load_content(&epub, &href_path, &mut content_cache) {
                    Ok(content) => content,
                    Err(err) => {
                        errors.push(err.to_string());
                        continue;
                    }
                };
                if options.markdown_mode == MarkdownMode::Rich {
                    collect_css(content, &href_path, &mut css_hrefs, &mut inline_styles);
                }
                let (text_opt, anchors) = render_partial_with_anchors(
                    content,
                    options.markdown_mode,
                    None,
                    None,
                    &mut image_resolver,
                );
                if let Some(text) = text_opt {
                    if !text.trim().is_empty() {
                        sections.push(SectionRecord {
                            title: label,
                            text,
                            start_href: href_path,
                            start_fragment: None,
                            end_href: None,
                            end_fragment: None,
                            spine_start: spine_index_by_href
                                .get(&content.href_path)
                                .copied()
                                .unwrap_or(0),
                            spine_end: spine_index_by_href
                                .get(&content.href_path)
                                .copied()
                                .unwrap_or(0),
                            anchors,
                            section_id: String::new(),
                            output_path: String::new(),
                        });
                    }
                }
            }
        }
    }

    if sections.is_empty() {
        anyhow::bail!("No readable sections found in {}", epub_path.display());
    }

    let mut cleanup_changes = 0usize;
    for section in &mut sections {
        section.section_id = build_section_id(
            &section.start_href,
            section.start_fragment.as_deref(),
            section.end_href.as_deref(),
            section.end_fragment.as_deref(),
        );
        let (cleaned, changes) = apply_ocr_cleanup(&section.text, options.ocr_cleanup);
        section.text = cleaned;
        cleanup_changes += changes;
    }

    if options.split_chapters {
        let width = std::cmp::max(2, sections.len().to_string().len());
        for (idx, section) in sections.iter_mut().enumerate() {
            let mut section_slug = if section.title.trim().is_empty() {
                format!("section_{:0width$}", idx + 1, width = width)
            } else {
                slugify(&section.title)
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
            section.output_path = match options.filename_scheme {
                FilenameScheme::Legacy => {
                    format!("{:0width$}_{}.md", idx + 1, section_slug, width = width)
                }
                FilenameScheme::Stable => format!("{}_{}.md", section.section_id, section_slug),
            };
        }
    } else {
        for section in &mut sections {
            section.output_path = format!("{book_slug}.md");
        }
    }

    let mut href_to_section: HashMap<String, usize> = HashMap::new();
    let mut anchor_to_section: HashMap<(String, String), usize> = HashMap::new();
    for (idx, section) in sections.iter().enumerate() {
        href_to_section
            .entry(section.start_href.clone())
            .or_insert(idx);
        if let Some(fragment) = &section.start_fragment {
            anchor_to_section.insert((section.start_href.clone(), fragment.clone()), idx);
        }
        for anchor in &section.anchors {
            anchor_to_section.insert((section.start_href.clone(), anchor.clone()), idx);
        }
    }

    let mut link_rewritten = 0usize;
    let mut link_unresolved = 0usize;
    for idx in 0..sections.len() {
        let base_href = sections[idx].start_href.clone();
        let replacer = |target: &str| -> (String, bool) {
            let Some((target_href, fragment)) = resolve_internal_target(target, &base_href) else {
                return (target.to_string(), true);
            };
            let mut target_idx = None;
            if let Some(frag) = &fragment {
                target_idx = anchor_to_section
                    .get(&(target_href.clone(), frag.clone()))
                    .copied();
            }
            if target_idx.is_none() {
                target_idx = href_to_section.get(&target_href).copied();
            }
            let Some(target_idx) = target_idx else {
                return (target.to_string(), false);
            };
            if options.split_chapters {
                if target_idx == idx {
                    if let Some(frag) = fragment {
                        return (format!("#{frag}"), true);
                    }
                    return (format!("./{}", sections[target_idx].output_path), true);
                }
                let mut out = format!("./{}", sections[target_idx].output_path);
                if let Some(frag) = fragment {
                    out.push('#');
                    out.push_str(&frag);
                }
                return (out, true);
            }
            if let Some(frag) = fragment {
                return (format!("#{frag}"), true);
            }
            (format!("#{}", sections[target_idx].section_id), true)
        };
        let (rewritten_md, md_rw, md_unresolved) =
            replace_markdown_links(&sections[idx].text, replacer);
        let (rewritten_html, html_rw, html_unresolved) =
            replace_html_links(&rewritten_md, replacer);
        sections[idx].text = rewritten_html;
        link_rewritten += md_rw + html_rw;
        link_unresolved += md_unresolved + html_unresolved;
    }
    if link_unresolved > 0 {
        warn(format!(
            "{}: unresolved internal links detected ({link_unresolved}).",
            title
        ));
    }

    let mut notes_written = 0usize;
    let mut global_note_lines: Vec<String> = Vec::new();
    if options.notes_mode != NotesMode::Inline {
        for section in &mut sections {
            let (stripped, notes) = extract_markdown_footnotes(&section.text);
            if notes.is_empty() {
                continue;
            }
            let mut id_map: HashMap<String, String> = HashMap::new();
            for (idx, (note_id, _)) in notes.iter().enumerate() {
                id_map.insert(
                    note_id.clone(),
                    format!("note-{}-{:03}", section.section_id, idx + 1),
                );
            }
            section.text = rewrite_note_refs(&stripped, &id_map);
            let rendered_defs: Vec<String> = notes
                .iter()
                .map(|(note_id, text)| {
                    format!("[^{}]: {}", id_map.get(note_id).unwrap_or(note_id), text)
                })
                .collect();
            notes_written += rendered_defs.len();
            match options.notes_mode {
                NotesMode::Inline => {}
                NotesMode::ChapterEnd => {
                    section.text = format!(
                        "{}\n\n### Notes\n\n{}",
                        section.text.trim(),
                        rendered_defs.join("\n")
                    );
                }
                NotesMode::Global => {
                    global_note_lines
                        .push(format!("## {} ({})", section.title, section.section_id));
                    global_note_lines.push(String::new());
                    global_note_lines.extend(rendered_defs);
                    global_note_lines.push(String::new());
                }
            }
        }
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
        book_dir.clone()
    } else {
        options.output_dir.clone()
    };
    fs::create_dir_all(&output_root)?;

    let mut base_lines = Vec::new();
    base_lines.push(format!("# {title}"));
    if let Some(ref author) = author {
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
        for section in &sections {
            let mut lines = base_lines.clone();
            lines.push(format!("<a id=\"{}\"></a>", section.section_id));
            lines.push(format!("## {}", section.title));
            lines.push(String::new());
            lines.push(section.text.clone());
            lines.push(String::new());
            fs::write(
                output_root.join(&section.output_path),
                lines.join("\n").trim().to_string() + "\n",
            )?;
        }
    } else {
        let output_path = output_root.join(format!("{book_slug}.md"));
        let mut lines = base_lines;
        for section in &sections {
            lines.push(format!("<a id=\"{}\"></a>", section.section_id));
            lines.push(format!("## {}", section.title));
            lines.push(String::new());
            lines.push(section.text.clone());
            lines.push(String::new());
        }
        if options.notes_mode == NotesMode::Global && !global_note_lines.is_empty() {
            lines.push("## Notes".to_string());
            lines.push(String::new());
            lines.extend(global_note_lines.clone());
        }
        fs::write(&output_path, lines.join("\n").trim().to_string() + "\n")?;
        return_path = output_path;
    }

    if options.notes_mode == NotesMode::Global && !global_note_lines.is_empty() {
        fs::create_dir_all(&book_dir)?;
        fs::write(
            book_dir.join("notes.md"),
            format!("# Notes\n\n{}\n", global_note_lines.join("\n").trim()),
        )?;
    }

    if extracted_count > 0 {
        println!("Extracted {extracted_count} images for {title}");
    }
    if extracted_media_count > 0 {
        println!("Extracted {extracted_media_count} media files for {title}");
    }

    if options.export_manifest == ExportMode::V1 {
        fs::create_dir_all(&book_dir)?;
        let sections_json: Vec<serde_json::Value> = sections
            .iter()
            .enumerate()
            .map(|(idx, section)| {
                json!({
                    "section_id": section.section_id,
                    "order": idx + 1,
                    "title": section.title,
                    "output_path": if options.split_chapters {
                        format!("{}/{}", book_slug, section.output_path)
                    } else {
                        section.output_path.clone()
                    },
                    "source_start": {
                        "href": section.start_href,
                        "fragment": section.start_fragment,
                        "spine_index": section.spine_start,
                    },
                    "source_end": {
                        "href": section.end_href,
                        "fragment": section.end_fragment,
                        "spine_index": section.spine_end,
                    },
                    "anchors": section.anchors,
                })
            })
            .collect();
        let toc_json: Vec<serde_json::Value> = toc_entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                json!({
                    "order": idx,
                    "label": entry.label,
                    "href": entry.href_path,
                    "fragment": entry.fragment
                })
            })
            .collect();
        let manifest_payload = json!({
            "schema_version": "v1",
            "book": {
                "title": title,
                "authors": author.clone().unwrap_or_default(),
                "slug": book_slug,
            },
            "spine": spine_hrefs.iter().enumerate().map(|(idx, href)| {
                json!({"index": idx, "href": href})
            }).collect::<Vec<_>>(),
            "toc_tree": toc_json,
            "sections": sections_json,
            "landmarks": [],
            "page_list": [],
            "assets": {
                "images": extracted_images.keys().collect::<Vec<_>>(),
                "media": extracted_media.keys().collect::<Vec<_>>(),
            },
            "build": {
                "markdown_mode": format!("{:?}", options.markdown_mode),
                "style": format!("{:?}", options.style),
                "split_chapters": options.split_chapters,
                "chapter_fallback": format!("{:?}", options.chapter_fallback),
                "notes_mode": format!("{:?}", options.notes_mode),
                "ocr_cleanup": format!("{:?}", options.ocr_cleanup),
                "nav_cleanup": format!("{:?}", options.nav_cleanup),
                "filename_scheme": format!("{:?}", options.filename_scheme),
            }
        });
        fs::write(
            book_dir.join("manifest.v1.json"),
            serde_json::to_string_pretty(&manifest_payload)? + "\n",
        )?;
    }

    if options.quality_report == ExportMode::V1 {
        fs::create_dir_all(&book_dir)?;
        let report = json!({
            "toc_stats": {
                "entries": toc_entry_count,
                "unique_hrefs": toc_unique_count,
                "coverage_ratio": toc_coverage_ratio,
                "degenerate": toc_is_degenerate,
            },
            "fallback_stats": {
                "mode": format!("{:?}", options.chapter_fallback),
                "used_heading_fallback": use_heading_fallback,
            },
            "link_stats": {
                "rewritten": link_rewritten,
                "unresolved": link_unresolved,
            },
            "asset_stats": {
                "images_extracted": extracted_count,
                "media_extracted": extracted_media_count,
                "missing_assets": warnings.iter().filter(|msg| msg.contains("missing media")).count(),
            },
            "ocr_stats": {
                "mode": format!("{:?}", options.ocr_cleanup),
                "cleanup_changes": cleanup_changes,
            },
            "cleanup_stats": {
                "nav_cleanup_mode": format!("{:?}", options.nav_cleanup),
                "toc_entries_removed": nav_removed,
            },
            "notes_stats": {
                "mode": format!("{:?}", options.notes_mode),
                "notes_written": notes_written,
            },
            "warnings": warnings,
            "errors": errors,
        });
        fs::write(
            book_dir.join("report.v1.json"),
            serde_json::to_string_pretty(&report)? + "\n",
        )?;
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
    let unique_toc_hrefs: HashSet<&str> = toc_entries
        .iter()
        .map(|entry| entry.href_path.as_str())
        .collect();
    let unique_count = unique_toc_hrefs.len();
    let coverage_ratio = if spine_doc_count > 0 {
        unique_count as f32 / spine_doc_count as f32
    } else {
        0.0
    };
    let is_degenerate = toc_entry_count <= 1 || unique_count < 3 || coverage_ratio < 0.15;
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
    let (top_window_text, first_nonempty_line, heading_texts) = extract_heading_features(content);

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
            if !first_nonempty_line.is_empty() && MAJOR_HEADING_RE.is_match(&first_nonempty_line) {
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
        .filter(|word| {
            word.chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
        })
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
    READABLE_MIME
        .iter()
        .any(|mime| mime.eq_ignore_ascii_case(media_type))
}

fn collect_css(
    content: &ContentDoc,
    base_href: &str,
    css_hrefs: &mut HashSet<String>,
    inline_styles: &mut Vec<String>,
) {
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

fn render_partial_with_anchors(
    content: &ContentDoc,
    markdown_mode: MarkdownMode,
    start_fragment: Option<&str>,
    end_fragment: Option<&str>,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> (Option<String>, Vec<String>) {
    if start_fragment.is_none() && end_fragment.is_none() {
        return (
            render_full_content(content, markdown_mode, image_resolver),
            collect_anchors_from_content(content),
        );
    }
    let body = match content.document.select_first("body") {
        Ok(node) => node.as_node().clone(),
        Err(_) => return (None, Vec::new()),
    };
    let children: Vec<NodeRef> = body.children().collect();
    if children.is_empty() {
        return (None, Vec::new());
    }
    let mut start_idx = 0usize;
    if let Some(fragment) = start_fragment {
        let Some(anchor) = find_anchor(&content.document, fragment) else {
            return (None, Vec::new());
        };
        let Some(top) = top_level_body_child(&body, &anchor) else {
            return (None, Vec::new());
        };
        let Some(idx) = child_index(&children, &top) else {
            return (None, Vec::new());
        };
        start_idx = idx;
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
        return (None, Vec::new());
    }
    let nodes = &children[start_idx..end_idx];
    (
        render_nodes_for_mode(nodes, content, markdown_mode, image_resolver),
        collect_anchors_from_nodes(nodes),
    )
}

fn collect_anchors_from_nodes(nodes: &[NodeRef]) -> Vec<String> {
    let mut anchors: HashSet<String> = HashSet::new();
    for node in nodes {
        if let Ok(matches) = node.select("[id]") {
            for n in matches {
                let attrs = n.attributes.borrow();
                if let Some(id) = attrs.get("id") {
                    if !id.trim().is_empty() {
                        anchors.insert(id.trim().to_string());
                    }
                }
            }
        }
        if let Ok(matches) = node.select("a[name]") {
            for n in matches {
                let attrs = n.attributes.borrow();
                if let Some(name) = attrs.get("name") {
                    if !name.trim().is_empty() {
                        anchors.insert(name.trim().to_string());
                    }
                }
            }
        }
    }
    let mut values: Vec<String> = anchors.into_iter().collect();
    values.sort();
    values
}

fn collect_anchors_from_content(content: &ContentDoc) -> Vec<String> {
    let Ok(body) = content.document.select_first("body") else {
        return Vec::new();
    };
    let nodes: Vec<NodeRef> = body.as_node().children().collect();
    collect_anchors_from_nodes(&nodes)
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
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
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

fn extract_media_file(
    epub: &Epub,
    resolved: &str,
    media_root: &Path,
    media_link_prefix: &str,
    extracted: &mut HashMap<String, String>,
    extracted_count: &mut usize,
) -> Option<String> {
    if let Some(existing) = extracted.get(resolved) {
        return Some(existing.clone());
    }
    let bytes = epub.read_resource_bytes(resolved).ok()?;
    let relative = decode_path(resolved);
    let output_path = media_root.join(&relative);
    if let Some(parent) = output_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&output_path, bytes).ok()?;
    *extracted_count += 1;
    let rel_path = format!("{media_link_prefix}/{relative}");
    extracted.insert(resolved.to_string(), rel_path.clone());
    Some(rel_path)
}

fn resolve_href(base_href: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        normalize_path(rel)
    } else {
        let base_dir = base_href.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
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

fn build_section_id(
    start_href: &str,
    start_fragment: Option<&str>,
    end_href: Option<&str>,
    end_fragment: Option<&str>,
) -> String {
    let canonical = format!(
        "{}#{}|{}#{}",
        start_href,
        start_fragment.unwrap_or(""),
        end_href.unwrap_or(""),
        end_fragment.unwrap_or("")
    );
    let mut hasher = Sha1::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    format!("{:x}", digest)[..12].to_string()
}

fn cleanup_toc_entries(
    entries: Vec<TocEntryInfo>,
    mode: NavCleanupMode,
) -> (Vec<TocEntryInfo>, usize) {
    if mode == NavCleanupMode::Off {
        return (entries, 0);
    }
    let mut cleaned: Vec<TocEntryInfo> = Vec::new();
    let mut removed = 0usize;
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for entry in entries {
        let label = normalize_space(&entry.label).to_lowercase();
        if label.is_empty() || OCR_NOISE_RE.is_match(&label) {
            removed += 1;
            continue;
        }
        let key = (
            entry.href_path.clone(),
            entry.fragment.clone().unwrap_or_default(),
            label,
        );
        if seen.contains(&key) {
            removed += 1;
            continue;
        }
        seen.insert(key);
        if let Some(prev) = cleaned.last() {
            if prev.href_path == entry.href_path && prev.fragment == entry.fragment {
                removed += 1;
                continue;
            }
        }
        cleaned.push(entry);
    }
    (cleaned, removed)
}

fn apply_ocr_cleanup(text: &str, mode: OcrCleanupMode) -> (String, usize) {
    if mode == OcrCleanupMode::Off {
        return (text.to_string(), 0);
    }
    let mut cleaned = text.to_string();
    let mut changes = 0usize;
    let hyphen_fixed = Regex::new(r"([A-Za-z])-\n([a-z])")
        .expect("regex")
        .replace_all(&cleaned, "$1$2")
        .to_string();
    if hyphen_fixed != cleaned {
        changes += 1;
        cleaned = hyphen_fixed;
    }
    let mut out = Vec::new();
    let mut prev = String::new();
    for line in cleaned.lines() {
        let stripped = line.trim();
        if OCR_NOISE_RE.is_match(stripped) {
            changes += 1;
            continue;
        }
        if mode == OcrCleanupMode::Aggressive && stripped.len() > 12 {
            let noise = stripped
                .chars()
                .filter(|c| {
                    !(c.is_ascii_alphanumeric()
                        || c.is_ascii_whitespace()
                        || ".,;:!?'-_()[]\"/".contains(*c))
                })
                .count();
            if (noise as f32) / (stripped.len() as f32) > 0.35 {
                changes += 1;
                continue;
            }
        }
        let compact = normalize_space(line);
        if !compact.is_empty() && compact == prev {
            changes += 1;
            continue;
        }
        out.push(line.to_string());
        if !compact.is_empty() {
            prev = compact;
        }
    }
    (out.join("\n").trim().to_string(), changes)
}

fn resolve_internal_target(target: &str, base_href: &str) -> Option<(String, Option<String>)> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("javascript:")
        || lower.starts_with("data:")
    {
        return None;
    }
    let (raw_path, fragment) = match trimmed.split_once('#') {
        Some((path, frag)) => (path, Some(frag.to_string())),
        None => (trimmed, None),
    };
    let href = if raw_path.is_empty() {
        normalize_path(base_href)
    } else {
        resolve_href(base_href, raw_path)
    };
    Some((href, fragment))
}

fn replace_markdown_links(
    input: &str,
    mut f: impl FnMut(&str) -> (String, bool),
) -> (String, usize, usize) {
    let mut rewritten = 0usize;
    let mut unresolved = 0usize;
    let output = MARKDOWN_LINK_RE
        .replace_all(input, |caps: &regex::Captures| {
            let bang = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let label = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let href = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            if bang == "!" {
                return format!("![{}]({})", label, href);
            }
            let (new_href, resolved) = f(href);
            if new_href != href {
                rewritten += 1;
            }
            if !resolved {
                unresolved += 1;
            }
            format!("[{}]({})", label, new_href)
        })
        .to_string();
    (output, rewritten, unresolved)
}

fn replace_html_links(
    input: &str,
    mut f: impl FnMut(&str) -> (String, bool),
) -> (String, usize, usize) {
    let mut rewritten = 0usize;
    let mut unresolved = 0usize;
    let output = HTML_HREF_RE
        .replace_all(input, |caps: &regex::Captures| {
            let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let href = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let suffix = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            let (new_href, resolved) = f(href);
            if new_href != href {
                rewritten += 1;
            }
            if !resolved {
                unresolved += 1;
            }
            format!("{prefix}{new_href}{suffix}")
        })
        .to_string();
    (output, rewritten, unresolved)
}

fn extract_markdown_footnotes(text: &str) -> (String, Vec<(String, String)>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    let mut notes: Vec<(String, String)> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let Some(caps) = FOOTNOTE_DEF_RE.captures(line) else {
            kept.push(line.to_string());
            i += 1;
            continue;
        };
        let id = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let mut payload = vec![
            caps.get(2)
                .map(|m| m.as_str())
                .unwrap_or("")
                .trim_end()
                .to_string(),
        ];
        i += 1;
        while i < lines.len() {
            let cont = lines[i];
            if cont.starts_with("    ") || cont.starts_with('\t') {
                payload.push(cont.trim_start().to_string());
                i += 1;
            } else {
                break;
            }
        }
        let value = payload.join("\n").trim().to_string();
        if !id.is_empty() && !value.is_empty() {
            notes.push((id, value));
        }
    }
    (kept.join("\n").trim().to_string(), notes)
}

fn rewrite_note_refs(text: &str, id_map: &HashMap<String, String>) -> String {
    if id_map.is_empty() {
        return text.to_string();
    }
    Regex::new(r"\[\^([^\]]+)\]")
        .expect("regex")
        .replace_all(text, |caps: &regex::Captures| {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let mapped = id_map.get(key).cloned().unwrap_or_else(|| key.to_string());
            format!("[^{}]", mapped)
        })
        .to_string()
}
