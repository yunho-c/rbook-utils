use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use rbook::Epub;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use kuchiki::NodeRef;

mod collect;
mod export;
mod heading;
mod postprocess;
mod render;
pub mod runtime;

use collect::{
    collect_image_hrefs, collect_media_hrefs, collect_readable_spine_docs, collect_toc_entries,
    load_content,
};
use export::{write_manifest_export, write_markdown_outputs, write_quality_report};
use heading::{detect_heading_candidates, prettify_section_name};
use postprocess::{cleanup_toc_entries, postprocess_sections};
use render::{
    build_style_header, collect_css, extract_image, extract_media_file,
    render_partial_with_anchors, resolve_and_extract_image,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FormatMode {
    Plain,
    Rich,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum CssMode {
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
    Index,
    Hash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum MediaMode {
    None,
    Image,
    All,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub media: MediaMode,
    pub format: FormatMode,
    pub css: CssMode,
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
    pub fn new(input: PathBuf, output: PathBuf) -> Self {
        Self {
            input,
            output,
            media: MediaMode::Image,
            format: FormatMode::Plain,
            css: CssMode::Inline,
            split_chapters: false,
            chapter_fallback: ChapterFallbackMode::Auto,
            notes_mode: NotesMode::Inline,
            export_manifest: ExportMode::Off,
            quality_report: ExportMode::Off,
            ocr_cleanup: OcrCleanupMode::Off,
            nav_cleanup: NavCleanupMode::Auto,
            filename_scheme: FilenameScheme::Index,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct BookConversionResult {
    pub input_path: PathBuf,
    pub title: String,
    pub output_path: Option<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Default)]
pub struct ConversionSummary {
    pub books: Vec<BookConversionResult>,
}

impl ConversionSummary {
    pub fn failure_count(&self) -> usize {
        self.books
            .iter()
            .filter(|book| book.output_path.is_none())
            .count()
    }

    pub fn success_count(&self) -> usize {
        self.books.len().saturating_sub(self.failure_count())
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
struct ReadableSpineDoc {
    href_path: String,
    label: String,
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

#[derive(Clone, Debug, Default)]
struct PostprocessStats {
    link_rewritten: usize,
    link_unresolved: usize,
    cleanup_changes: usize,
    notes_written: usize,
    global_note_lines: Vec<String>,
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

pub fn convert_all(options: &ConvertOptions) -> Result<ConversionSummary> {
    let epub_paths = collect_input_epubs(&options.input)?;

    let mut summary = ConversionSummary::default();
    for epub_path in epub_paths {
        match convert_epub_result(&epub_path, options) {
            Ok(result) => summary.books.push(result),
            Err(err) => {
                summary.books.push(BookConversionResult {
                    input_path: epub_path.clone(),
                    title: epub_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("book")
                        .to_string(),
                    output_path: None,
                    diagnostics: vec![Diagnostic {
                        level: DiagnosticLevel::Error,
                        message: format!("Failed to parse {}: {err}", epub_path.display()),
                    }],
                });
            }
        }
    }

    Ok(summary)
}

pub fn convert_epub(epub_path: &Path, options: &ConvertOptions) -> Result<PathBuf> {
    let result = convert_epub_result(epub_path, options)?;
    result
        .output_path
        .ok_or_else(|| anyhow::anyhow!("No output path generated for {}", epub_path.display()))
}

pub fn convert_epub_result(
    epub_path: &Path,
    options: &ConvertOptions,
) -> Result<BookConversionResult> {
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
    let book_dir = options.output.join(&book_slug);
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
        warnings.push(message);
    };

    if options.media == MediaMode::All {
        for href in collect_image_hrefs(&epub) {
            let _ = extract_image(
                &epub,
                &href,
                &image_root,
                &image_link_prefix,
                &mut extracted_images,
                &mut extracted_count,
            );
        }
        for href in collect_media_hrefs(&epub) {
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
        match options.media {
            MediaMode::None => Some(src.to_string()),
            MediaMode::Image | MediaMode::All => resolve_and_extract_image(
                &epub,
                src,
                base_href,
                &image_root,
                &image_link_prefix,
                &mut extracted_images,
                &mut extracted_count,
            ),
        }
    };

    let toc_entries_raw = collect_toc_entries(&epub);
    let (toc_entries, nav_removed) = cleanup_toc_entries(toc_entries_raw, options.nav_cleanup);
    let spine_docs = collect_readable_spine_docs(&epub);
    let spine_hrefs: Vec<String> = spine_docs.iter().map(|doc| doc.href_path.clone()).collect();
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
                    if options.format == FormatMode::Rich {
                        collect_css(content, href, &mut css_hrefs, &mut inline_styles);
                    }
                    let (part, part_anchors) = render_partial_with_anchors(
                        content,
                        options.format,
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
                if options.format == FormatMode::Rich {
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
                    options.format,
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
        for spine_doc in &spine_docs {
            let href_path = spine_doc.href_path.clone();
            let label = spine_doc.label.clone();
            let content = match load_content(&epub, &href_path, &mut content_cache) {
                Ok(content) => content,
                Err(err) => {
                    errors.push(err.to_string());
                    continue;
                }
            };
            if options.format == FormatMode::Rich {
                collect_css(content, &href_path, &mut css_hrefs, &mut inline_styles);
            }
            let (text_opt, anchors) = render_partial_with_anchors(
                content,
                options.format,
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

    if sections.is_empty() {
        anyhow::bail!("No readable sections found in {}", epub_path.display());
    }

    let stats = postprocess_sections(
        &mut sections,
        options.split_chapters,
        options.filename_scheme,
        &book_slug,
        options.ocr_cleanup,
        options.notes_mode,
    );
    if stats.link_unresolved > 0 {
        warn(format!(
            "{}: unresolved internal links detected ({}).",
            title, stats.link_unresolved
        ));
    }

    let style_header_lines = if options.format == FormatMode::Rich {
        build_style_header(
            &epub,
            &css_hrefs,
            &inline_styles,
            &style_root,
            &style_link_prefix,
            options.css,
        )?
    } else {
        Vec::new()
    };

    let return_path = write_markdown_outputs(
        &sections,
        options,
        &options.output,
        &book_dir,
        &book_slug,
        &title,
        author.as_ref(),
        &style_header_lines,
        &stats.global_note_lines,
    )?;

    write_manifest_export(
        options.export_manifest,
        &book_dir,
        &title,
        author.as_ref(),
        &book_slug,
        &spine_hrefs,
        &toc_entries,
        &sections,
        &extracted_images,
        &extracted_media,
        options,
    )?;
    write_quality_report(
        options.quality_report,
        &book_dir,
        toc_entry_count,
        toc_unique_count,
        toc_coverage_ratio,
        toc_is_degenerate,
        use_heading_fallback,
        options,
        &stats,
        extracted_count,
        extracted_media_count,
        nav_removed,
        &warnings,
        &errors,
    )?;

    let mut diagnostics = Vec::new();
    if extracted_count > 0 {
        diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: format!("Extracted {extracted_count} images for {title}"),
        });
    }
    if extracted_media_count > 0 {
        diagnostics.push(Diagnostic {
            level: DiagnosticLevel::Info,
            message: format!("Extracted {extracted_media_count} media files for {title}"),
        });
    }
    diagnostics.extend(warnings.into_iter().map(|message| Diagnostic {
        level: DiagnosticLevel::Warning,
        message,
    }));
    diagnostics.extend(errors.into_iter().map(|message| Diagnostic {
        level: DiagnosticLevel::Error,
        message,
    }));

    Ok(BookConversionResult {
        input_path: epub_path.to_path_buf(),
        title,
        output_path: Some(return_path),
        diagnostics,
    })
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

fn collect_input_epubs(input: &Path) -> Result<Vec<PathBuf>> {
    let metadata = std::fs::metadata(input)
        .with_context(|| format!("Failed to access {}", input.display()))?;

    if metadata.is_file() {
        if input.extension().and_then(|ext| ext.to_str()) == Some("epub") {
            return Ok(vec![input.to_path_buf()]);
        }
        anyhow::bail!(
            "Input path {} is a file, but not an .epub file",
            input.display()
        );
    }

    if !metadata.is_dir() {
        anyhow::bail!(
            "Input path {} is neither a regular file nor a directory",
            input.display()
        );
    }

    let mut epub_paths = Vec::new();
    for entry in WalkDir::new(input)
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
        anyhow::bail!("No EPUB files found under {}", input.display());
    }

    Ok(epub_paths)
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
