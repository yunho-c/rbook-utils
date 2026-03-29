use anyhow::{Context, Result};
use kuchiki::NodeRef;
use rbook::Epub;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::collect::{collect_readable_spine_docs, collect_toc_entries, load_content};
use super::heading::{detect_heading_candidates, prettify_section_name};
use super::postprocess::cleanup_toc_entries;
use super::render::{collect_anchors_from_nodes, slice_content_nodes};
use super::{normalize_space, resolve_href, toc_degeneracy_stats, ContentDoc, NavCleanupMode, TocEntryInfo};

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeParsedEpubBook {
    pub metadata: RuntimeEpubMetadata,
    pub toc: Vec<RuntimeTocEntry>,
    pub cover_image: Option<Vec<u8>>,
    pub sections: Vec<RuntimeSection>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeEpubMetadata {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeTocEntry {
    pub title: String,
    pub href: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSection {
    pub id: String,
    pub title: String,
    pub start_href: String,
    pub start_fragment: Option<String>,
    pub end_href: Option<String>,
    pub end_fragment: Option<String>,
    pub spine_start: usize,
    pub spine_end: usize,
    pub anchors: Vec<String>,
    pub document: RuntimeReaderDocument,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeReaderDocument {
    pub chapter_href: String,
    pub blocks: Vec<RuntimeReaderBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeReaderBlockKind {
    Paragraph,
    Heading,
    BlockQuote,
    List,
    ListItem,
    Image,
    Table,
    HorizontalRule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeReaderBlock {
    pub kind: RuntimeReaderBlockKind,
    pub level: i32,
    pub ordered: bool,
    pub inlines: Vec<RuntimeReaderInline>,
    pub blocks: Vec<RuntimeReaderBlock>,
    pub items: Vec<RuntimeListItem>,
    pub src: Option<String>,
    pub alt: Option<String>,
    pub caption: Option<String>,
    pub rows: Vec<RuntimeTableRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeListItem {
    pub blocks: Vec<RuntimeReaderBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeTableRow {
    pub cells: Vec<RuntimeTableCell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeTableCell {
    pub is_header: bool,
    pub inlines: Vec<RuntimeReaderInline>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeReaderInlineKind {
    Text,
    Emphasis,
    Strong,
    Sup,
    Sub,
    Link,
    LineBreak,
    Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeSpanStyleHint {
    Italic,
    Bold,
    Underline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeReaderInline {
    pub kind: RuntimeReaderInlineKind,
    pub text: Option<String>,
    pub href: Option<String>,
    pub style_hints: Vec<RuntimeSpanStyleHint>,
    pub children: Vec<RuntimeReaderInline>,
}

#[derive(Clone, Debug)]
struct PlannedSection {
    title: String,
    start_href: String,
    start_fragment: Option<String>,
    end_href: Option<String>,
    end_fragment: Option<String>,
    spine_start: usize,
    spine_end: usize,
}

pub fn parse_epub_runtime(epub_path: &Path) -> Result<RuntimeParsedEpubBook> {
    let epub = Epub::open(epub_path)
        .with_context(|| format!("Failed to open epub {}", epub_path.display()))?;
    parse_epub_runtime_from_book(&epub)
}

pub fn read_epub_resource_bytes(epub_path: &Path, href: &str) -> Result<Option<Vec<u8>>> {
    let normalized = normalize_resource_href(href);
    if normalized.is_empty() {
        return Ok(None);
    }
    let epub = Epub::open(epub_path)
        .with_context(|| format!("Failed to open epub {}", epub_path.display()))?;
    Ok(epub.read_resource_bytes(&normalized).ok())
}

fn parse_epub_runtime_from_book(epub: &Epub) -> Result<RuntimeParsedEpubBook> {
    let epub_metadata = epub.metadata();
    let metadata = RuntimeEpubMetadata {
        title: epub_metadata.title().map(|t| t.value().to_string()),
        creator: epub_metadata.creators().next().map(|c| c.value().to_string()),
        language: epub_metadata.language().map(|l| l.value().to_string()),
        identifier: epub_metadata.identifier().map(|i| i.value().to_string()),
        publisher: epub_metadata.publishers().next().map(|p| p.value().to_string()),
        description: epub_metadata.description().map(|d| d.value().to_string()),
    };

    let toc_entries_raw = collect_toc_entries(epub);
    let (toc_entries, _) = cleanup_toc_entries(toc_entries_raw, NavCleanupMode::Auto);
    let toc = toc_entries
        .iter()
        .map(|entry| RuntimeTocEntry {
            title: entry.title_or_fallback(),
            href: runtime_toc_href(entry),
        })
        .collect();

    let sections = plan_sections(epub, &toc_entries)?
        .into_iter()
        .map(|planned| build_runtime_section(epub, planned))
        .collect::<Result<Vec<_>>>()?;

    Ok(RuntimeParsedEpubBook {
        metadata,
        toc,
        cover_image: epub.manifest().cover_image().and_then(|cover| cover.read_bytes().ok()),
        sections,
    })
}

fn plan_sections(epub: &Epub, toc_entries: &[TocEntryInfo]) -> Result<Vec<PlannedSection>> {
    let spine_docs = collect_readable_spine_docs(epub);
    let spine_hrefs: Vec<String> = spine_docs.iter().map(|doc| doc.href_path.clone()).collect();
    if spine_hrefs.is_empty() {
        anyhow::bail!("No readable spine documents found");
    }
    let spine_index_by_href: HashMap<String, usize> = spine_hrefs
        .iter()
        .enumerate()
        .map(|(idx, href)| (href.clone(), idx))
        .collect();
    let (toc_is_degenerate, _, _, _) = toc_degeneracy_stats(toc_entries, spine_hrefs.len());

    if !toc_entries.is_empty() && !toc_is_degenerate {
        let mut sections = Vec::new();
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
            sections.push(PlannedSection {
                title: entry.title_or_fallback(),
                start_href: entry.href_path.clone(),
                start_fragment: entry.fragment.clone(),
                end_href: next_entry.map(|next| next.href_path.clone()),
                end_fragment: next_entry.and_then(|next| next.fragment.clone()),
                spine_start: start_idx,
                spine_end: end_idx,
            });
        }
        if !sections.is_empty() {
            return Ok(sections);
        }
    }

    let mut content_cache: HashMap<String, ContentDoc> = HashMap::new();
    let heading_candidates = detect_heading_candidates(&spine_hrefs, &mut content_cache, epub);
    let confident_candidates: Vec<_> = heading_candidates
        .into_iter()
        .filter(|candidate| candidate.spine_idx > 0)
        .collect();
    if !confident_candidates.is_empty() {
        let first_label = toc_entries
            .first()
            .map(TocEntryInfo::title_or_fallback)
            .unwrap_or_else(|| {
                spine_hrefs
                    .first()
                    .map(|href| prettify_section_name(href))
                    .unwrap_or_else(|| "Section 1".to_string())
            });
        let mut starts: Vec<(usize, String)> = vec![(0, first_label)];
        for candidate in confident_candidates {
            let label = if candidate.label.trim().is_empty() {
                format!("Section {}", starts.len() + 1)
            } else {
                candidate.label
            };
            starts.push((candidate.spine_idx, label));
        }

        let mut sections = Vec::new();
        for (start_pos, (start_idx, title)) in starts.iter().enumerate() {
            let next_start = starts
                .get(start_pos + 1)
                .map(|(idx, _)| *idx)
                .unwrap_or(spine_hrefs.len());
            if next_start == 0 || next_start <= *start_idx {
                continue;
            }
            let end_idx = next_start - 1;
            sections.push(PlannedSection {
                title: title.clone(),
                start_href: spine_hrefs[*start_idx].clone(),
                start_fragment: None,
                end_href: Some(spine_hrefs[end_idx].clone()),
                end_fragment: None,
                spine_start: *start_idx,
                spine_end: end_idx,
            });
        }
        if !sections.is_empty() {
            return Ok(sections);
        }
    }

    Ok(spine_docs
        .iter()
        .map(|doc| {
            let spine_idx = spine_index_by_href.get(&doc.href_path).copied().unwrap_or(0);
            PlannedSection {
                title: prettify_section_name(&doc.label),
                start_href: doc.href_path.clone(),
                start_fragment: None,
                end_href: None,
                end_fragment: None,
                spine_start: spine_idx,
                spine_end: spine_idx,
            }
        })
        .collect())
}

fn build_runtime_section(epub: &Epub, planned: PlannedSection) -> Result<RuntimeSection> {
    let spine_docs = collect_readable_spine_docs(epub);
    let spine_hrefs: Vec<String> = spine_docs.iter().map(|doc| doc.href_path.clone()).collect();
    let mut content_cache: HashMap<String, ContentDoc> = HashMap::new();
    let mut blocks = Vec::new();
    let mut anchors = HashSet::new();

    for spine_idx in planned.spine_start..=planned.spine_end {
        let Some(href) = spine_hrefs.get(spine_idx) else {
            continue;
        };
        let content = load_content(epub, href, &mut content_cache)?;
        let start_fragment = if spine_idx == planned.spine_start {
            planned.start_fragment.as_deref()
        } else {
            None
        };
        let end_fragment = if spine_idx == planned.spine_end {
            planned.end_fragment.as_deref()
        } else {
            None
        };
        let nodes = slice_content_nodes(content, start_fragment, end_fragment);
        for anchor in collect_anchors_from_nodes(&nodes) {
            anchors.insert(anchor);
        }
        blocks.extend(parse_blocks(&nodes, href));
    }

    let mut sorted_anchors: Vec<String> = anchors.into_iter().collect();
    sorted_anchors.sort();
    let title = if planned.title.trim().is_empty() {
        prettify_section_name(&planned.start_href)
    } else {
        planned.title
    };

    Ok(RuntimeSection {
        id: build_section_id(
            &planned.start_href,
            planned.start_fragment.as_deref(),
            planned.end_href.as_deref(),
            planned.end_fragment.as_deref(),
        ),
        title,
        start_href: planned.start_href.clone(),
        start_fragment: planned.start_fragment,
        end_href: planned.end_href,
        end_fragment: planned.end_fragment,
        spine_start: planned.spine_start,
        spine_end: planned.spine_end,
        anchors: sorted_anchors,
        document: RuntimeReaderDocument {
            chapter_href: planned.start_href,
            blocks,
        },
    })
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

fn parse_blocks(nodes: &[NodeRef], chapter_href: &str) -> Vec<RuntimeReaderBlock> {
    let mut blocks = Vec::new();
    for node in nodes {
        if let Some(text) = node.as_text() {
            let normalized = normalize_text(&text.borrow());
            if !normalized.trim().is_empty() {
                blocks.push(RuntimeReaderBlock {
                    kind: RuntimeReaderBlockKind::Paragraph,
                    level: 0,
                    ordered: false,
                    inlines: vec![RuntimeReaderInline::text(normalized)],
                    blocks: Vec::new(),
                    items: Vec::new(),
                    src: None,
                    alt: None,
                    caption: None,
                    rows: Vec::new(),
                });
            }
            continue;
        }
        if let Some(element) = node.as_element() {
            blocks.extend(parse_element_block(node, element.name.local.as_ref(), chapter_href));
        }
    }
    blocks
}

fn parse_element_block(node: &NodeRef, tag: &str, chapter_href: &str) -> Vec<RuntimeReaderBlock> {
    match tag {
        "p" => vec![RuntimeReaderBlock::paragraph(parse_inlines(node, chapter_href))],
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = tag
                .strip_prefix('h')
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(1)
                .clamp(1, 6);
            vec![RuntimeReaderBlock::heading(level, parse_inlines(node, chapter_href))]
        }
        "blockquote" => vec![RuntimeReaderBlock::blockquote(parse_blocks(
            &node.children().collect::<Vec<_>>(),
            chapter_href,
        ))],
        "ul" | "ol" => {
            let ordered = tag == "ol";
            let items = node
                .children()
                .filter_map(|child| {
                    let child_tag = child.as_element()?.name.local.to_string();
                    if child_tag != "li" {
                        return None;
                    }
                    Some(RuntimeListItem {
                        blocks: parse_blocks(&child.children().collect::<Vec<_>>(), chapter_href),
                    })
                })
                .collect::<Vec<_>>();
            vec![RuntimeReaderBlock::list(ordered, items)]
        }
        "li" => vec![RuntimeReaderBlock::paragraph(parse_inlines(node, chapter_href))],
        "figure" => parse_figure(node, chapter_href),
        "img" => parse_image_block(node, chapter_href).into_iter().collect(),
        "table" => vec![parse_table(node, chapter_href)],
        "hr" => vec![RuntimeReaderBlock::horizontal_rule()],
        "br" | "script" | "style" => Vec::new(),
        _ => {
            let nested = parse_blocks(&node.children().collect::<Vec<_>>(), chapter_href);
            if !nested.is_empty() {
                nested
            } else {
                let inlines = parse_inlines(node, chapter_href);
                if inlines.is_empty() {
                    Vec::new()
                } else {
                    vec![RuntimeReaderBlock::paragraph(inlines)]
                }
            }
        }
    }
}

fn parse_figure(node: &NodeRef, chapter_href: &str) -> Vec<RuntimeReaderBlock> {
    let mut image_node = None;
    let mut caption = None;
    for child in node.children() {
        let Some(element) = child.as_element() else {
            continue;
        };
        let tag = element.name.local.as_ref();
        if tag == "img" && image_node.is_none() {
            image_node = Some(child.clone());
        } else if tag == "figcaption" && caption.is_none() {
            let text = normalize_space(&child.text_contents());
            if !text.is_empty() {
                caption = Some(text);
            }
        }
    }
    let Some(image_node) = image_node else {
        let inlines = parse_inlines(node, chapter_href);
        if inlines.is_empty() {
            return Vec::new();
        }
        return vec![RuntimeReaderBlock::paragraph(inlines)];
    };
    parse_image_block(&image_node, chapter_href)
        .map(|mut block| {
            block.caption = caption;
            block
        })
        .into_iter()
        .collect()
}

fn parse_image_block(node: &NodeRef, chapter_href: &str) -> Option<RuntimeReaderBlock> {
    let element = node.as_element()?;
    let attrs = element.attributes.borrow();
    let src = attrs.get("src")?.trim();
    if src.is_empty() {
        return None;
    }
    Some(RuntimeReaderBlock::image(
        resolve_href(chapter_href, src),
        attrs.get("alt").map(|value| value.trim().to_string()).filter(|value| !value.is_empty()),
        None,
    ))
}

fn parse_table(node: &NodeRef, chapter_href: &str) -> RuntimeReaderBlock {
    let mut rows = Vec::new();
    if let Ok(matches) = node.select("tr") {
        for row in matches {
            let mut cells = Vec::new();
            for child in row.as_node().children() {
                let Some(element) = child.as_element() else {
                    continue;
                };
                let tag = element.name.local.as_ref();
                if tag != "td" && tag != "th" {
                    continue;
                }
                cells.push(RuntimeTableCell {
                    is_header: tag == "th",
                    inlines: parse_inlines(&child, chapter_href),
                });
            }
            if !cells.is_empty() {
                rows.push(RuntimeTableRow { cells });
            }
        }
    }
    RuntimeReaderBlock::table(rows)
}

fn parse_inlines(node: &NodeRef, chapter_href: &str) -> Vec<RuntimeReaderInline> {
    let mut inlines = Vec::new();
    for child in node.children() {
        if let Some(text) = child.as_text() {
            let normalized = normalize_text(&text.borrow());
            if !normalized.trim().is_empty() {
                inlines.push(RuntimeReaderInline::text(normalized));
            }
            continue;
        }
        let Some(element) = child.as_element() else {
            continue;
        };
        match element.name.local.as_ref() {
            "br" => inlines.push(RuntimeReaderInline::line_break()),
            "em" | "i" => inlines.push(RuntimeReaderInline::emphasis(parse_inlines(
                &child,
                chapter_href,
            ))),
            "strong" | "b" => inlines.push(RuntimeReaderInline::strong(parse_inlines(
                &child,
                chapter_href,
            ))),
            "sup" => inlines.push(RuntimeReaderInline::sup(parse_inlines(&child, chapter_href))),
            "sub" => inlines.push(RuntimeReaderInline::sub(parse_inlines(&child, chapter_href))),
            "a" => {
                let attrs = element.attributes.borrow();
                let href = attrs
                    .get("href")
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .map(|value| resolve_href(chapter_href, value))
                    .unwrap_or_default();
                inlines.push(RuntimeReaderInline::link(href, parse_inlines(&child, chapter_href)));
            }
            "span" => {
                let attrs = element.attributes.borrow();
                let hints = parse_style_hints(attrs.get("style"));
                let children = parse_inlines(&child, chapter_href);
                if hints.is_empty() {
                    inlines.extend(children);
                } else {
                    inlines.push(RuntimeReaderInline::span(hints, children));
                }
            }
            "img" => {
                let attrs = element.attributes.borrow();
                if let Some(alt) = attrs
                    .get("alt")
                    .map(|value| normalize_space(value))
                    .filter(|value| !value.is_empty())
                {
                    inlines.push(RuntimeReaderInline::text(alt));
                }
            }
            "script" | "style" => {}
            _ => inlines.extend(parse_inlines(&child, chapter_href)),
        }
    }
    inlines
}

fn parse_style_hints(style: Option<&str>) -> Vec<RuntimeSpanStyleHint> {
    let Some(style) = style else {
        return Vec::new();
    };
    let normalized = style.to_lowercase();
    let mut hints = Vec::new();
    if normalized.contains("font-style: italic") {
        hints.push(RuntimeSpanStyleHint::Italic);
    }
    if normalized.contains("font-weight: bold") || normalized.contains("font-weight: 700") {
        hints.push(RuntimeSpanStyleHint::Bold);
    }
    if normalized.contains("text-decoration: underline") {
        hints.push(RuntimeSpanStyleHint::Underline);
    }
    hints
}

fn normalize_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn runtime_toc_href(entry: &TocEntryInfo) -> String {
    let fragment = entry.fragment.clone().unwrap_or_default();
    if fragment.is_empty() {
        entry.href_path.clone()
    } else {
        format!("{}#{fragment}", entry.href_path)
    }
}

fn normalize_resource_href(href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let without_fragment = trimmed.split('#').next().unwrap_or("").trim();
    if without_fragment.is_empty() {
        return String::new();
    }
    let decoded = urlencoding::decode(without_fragment)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| without_fragment.to_string());
    super::normalize_path(&decoded)
}

impl RuntimeReaderBlock {
    fn paragraph(inlines: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderBlockKind::Paragraph,
            level: 0,
            ordered: false,
            inlines,
            blocks: Vec::new(),
            items: Vec::new(),
            src: None,
            alt: None,
            caption: None,
            rows: Vec::new(),
        }
    }

    fn heading(level: i32, inlines: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderBlockKind::Heading,
            level,
            ordered: false,
            inlines,
            blocks: Vec::new(),
            items: Vec::new(),
            src: None,
            alt: None,
            caption: None,
            rows: Vec::new(),
        }
    }

    fn blockquote(blocks: Vec<RuntimeReaderBlock>) -> Self {
        Self {
            kind: RuntimeReaderBlockKind::BlockQuote,
            level: 0,
            ordered: false,
            inlines: Vec::new(),
            blocks,
            items: Vec::new(),
            src: None,
            alt: None,
            caption: None,
            rows: Vec::new(),
        }
    }

    fn list(ordered: bool, items: Vec<RuntimeListItem>) -> Self {
        Self {
            kind: RuntimeReaderBlockKind::List,
            level: 0,
            ordered,
            inlines: Vec::new(),
            blocks: Vec::new(),
            items,
            src: None,
            alt: None,
            caption: None,
            rows: Vec::new(),
        }
    }

    fn image(src: String, alt: Option<String>, caption: Option<String>) -> Self {
        Self {
            kind: RuntimeReaderBlockKind::Image,
            level: 0,
            ordered: false,
            inlines: Vec::new(),
            blocks: Vec::new(),
            items: Vec::new(),
            src: Some(src),
            alt,
            caption,
            rows: Vec::new(),
        }
    }

    fn table(rows: Vec<RuntimeTableRow>) -> Self {
        Self {
            kind: RuntimeReaderBlockKind::Table,
            level: 0,
            ordered: false,
            inlines: Vec::new(),
            blocks: Vec::new(),
            items: Vec::new(),
            src: None,
            alt: None,
            caption: None,
            rows,
        }
    }

    fn horizontal_rule() -> Self {
        Self {
            kind: RuntimeReaderBlockKind::HorizontalRule,
            level: 0,
            ordered: false,
            inlines: Vec::new(),
            blocks: Vec::new(),
            items: Vec::new(),
            src: None,
            alt: None,
            caption: None,
            rows: Vec::new(),
        }
    }
}

impl RuntimeReaderInline {
    fn text(text: String) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Text,
            text: Some(text),
            href: None,
            style_hints: Vec::new(),
            children: Vec::new(),
        }
    }

    fn emphasis(children: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Emphasis,
            text: None,
            href: None,
            style_hints: Vec::new(),
            children,
        }
    }

    fn strong(children: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Strong,
            text: None,
            href: None,
            style_hints: Vec::new(),
            children,
        }
    }

    fn sup(children: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Sup,
            text: None,
            href: None,
            style_hints: Vec::new(),
            children,
        }
    }

    fn sub(children: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Sub,
            text: None,
            href: None,
            style_hints: Vec::new(),
            children,
        }
    }

    fn link(href: String, children: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Link,
            text: None,
            href: Some(href),
            style_hints: Vec::new(),
            children,
        }
    }

    fn line_break() -> Self {
        Self {
            kind: RuntimeReaderInlineKind::LineBreak,
            text: None,
            href: None,
            style_hints: Vec::new(),
            children: Vec::new(),
        }
    }

    fn span(style_hints: Vec<RuntimeSpanStyleHint>, children: Vec<RuntimeReaderInline>) -> Self {
        Self {
            kind: RuntimeReaderInlineKind::Span,
            text: None,
            href: None,
            style_hints,
            children,
        }
    }
}

trait TocEntryRuntimeExt {
    fn title_or_fallback(&self) -> String;
}

impl TocEntryRuntimeExt for TocEntryInfo {
    fn title_or_fallback(&self) -> String {
        let normalized = normalize_space(&self.label);
        if normalized.is_empty() {
            prettify_section_name(&self.href_path)
        } else {
            normalized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/books").join(name)
    }

    #[test]
    fn parses_sections_and_ir_from_toc() {
        let book = parse_epub_runtime(&fixture("Alice's Adventures in Wonderland.epub"))
            .expect("parse alice");
        assert!(!book.sections.is_empty());
        assert!(!book.toc.is_empty());
        assert!(!book.sections[0].document.blocks.is_empty());
        assert!(book
            .sections
            .iter()
            .any(|section| section.title.to_lowercase().contains("chapter")));
    }

    #[test]
    fn supports_fragment_slicing_and_heading_fallback() {
        let book = parse_epub_runtime(
            &fixture("Marcus Aurelius, Gregory Hays - Meditations_ A New Translation-Modern Library (2003).epub"),
        )
        .expect("parse meditations");
        assert!(book.sections.len() > 3);
        assert!(book.sections.iter().all(|section| !section.title.trim().is_empty()));
    }

    #[test]
    fn reads_image_resources() {
        let book = parse_epub_runtime(&fixture("Alice's Adventures in Wonderland.epub"))
            .expect("parse alice");
        let image_block = book
            .sections
            .iter()
            .flat_map(|section| section.document.blocks.iter())
            .find_map(|block| block.src.clone());
        let href = image_block.expect("image block");
        let bytes = read_epub_resource_bytes(&fixture("Alice's Adventures in Wonderland.epub"), &href)
            .expect("read resource");
        assert!(bytes.is_some());
        assert!(!bytes.unwrap().is_empty());
    }
}
