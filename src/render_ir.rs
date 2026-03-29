use anyhow::{Context, Result};
use kuchiki::NodeRef;
use rbook::Epub;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::collect::{collect_readable_spine_docs, collect_toc_entries, load_content};
use super::dom::{
    body_node, collect_anchors_from_nodes, nearest_fragment, node_path_from_root,
    slice_content_nodes,
};
use super::heading::{detect_heading_candidates, prettify_section_name};
use super::postprocess::cleanup_toc_entries;
use super::{
    ContentDoc, NavCleanupMode, TocEntryInfo, is_external, normalize_path, normalize_space,
    resolve_href, toc_degeneracy_stats,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookRenderModel {
    pub metadata: RenderMetadata,
    pub toc: Vec<TocEntry>,
    pub cover_image: Option<ResourceRef>,
    pub stylesheets: Vec<StylesheetInput>,
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderMetadata {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    pub target: LinkTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: String,
    pub title: Option<String>,
    pub source: SectionSourceRange,
    pub anchors: Vec<String>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionSourceRange {
    pub start_href: String,
    pub start_fragment: Option<String>,
    pub end_href: Option<String>,
    pub end_fragment: Option<String>,
    pub spine_start: usize,
    pub spine_end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Paragraph),
    Heading(Heading),
    Quote(QuoteBlock),
    List(ListBlock),
    Table(TableBlock),
    Image(ImageBlock),
    Rule(RuleBlock),
    Code(CodeBlock),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub inlines: Vec<Inline>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub inlines: Vec<Inline>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteBlock {
    pub blocks: Vec<Block>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListBlock {
    pub ordered: bool,
    pub items: Vec<ListItem>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableBlock {
    pub rows: Vec<TableRow>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCell {
    pub is_header: bool,
    pub inlines: Vec<Inline>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBlock {
    pub resource: ResourceRef,
    pub alt: Option<String>,
    pub caption: Option<String>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleBlock {
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeBlock {
    pub text: String,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text(TextRun),
    Span(Span),
    Link(LinkSpan),
    Image(ImageInline),
    Break(SourceMap),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRun {
    pub text: String,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub children: Vec<Inline>,
    pub styles: Vec<TextStyleHint>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub target: LinkTarget,
    pub children: Vec<Inline>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInline {
    pub resource: ResourceRef,
    pub alt: Option<String>,
    pub source: SourceMap,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextStyleHint {
    Italic,
    Bold,
    Underline,
    Superscript,
    Subscript,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Internal {
        section_id: String,
        section_index: usize,
        href: String,
        fragment: Option<String>,
    },
    External {
        href: String,
    },
    Unresolved {
        href: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMap {
    pub spine_index: usize,
    pub href: String,
    pub fragment: Option<String>,
    pub node_path: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Image,
    Stylesheet,
    Cover,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRef {
    pub href: String,
    pub media_type: Option<String>,
    pub kind: ResourceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StylesheetInput {
    Linked(ResourceRef),
    Inline { css_text: String, source: SourceMap },
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

#[derive(Clone, Debug)]
struct SectionDraft {
    section: Section,
    hrefs: Vec<String>,
    targets: HashSet<String>,
}

#[derive(Clone)]
struct LoweringContext {
    root: NodeRef,
    href: String,
    spine_index: usize,
}

pub fn parse_epub_render_model(epub_path: &Path) -> Result<BookRenderModel> {
    let epub = Epub::open(epub_path)
        .with_context(|| format!("Failed to open epub {}", epub_path.display()))?;
    parse_epub_render_model_from_book(&epub)
}

pub fn read_epub_resource_bytes(
    epub_path: &Path,
    resource: &ResourceRef,
) -> Result<Option<Vec<u8>>> {
    let normalized = normalize_resource_href(&resource.href);
    if normalized.is_empty() || is_external(&normalized) {
        return Ok(None);
    }
    let epub = Epub::open(epub_path)
        .with_context(|| format!("Failed to open epub {}", epub_path.display()))?;
    Ok(epub.read_resource_bytes(&normalized).ok())
}

fn parse_epub_render_model_from_book(epub: &Epub) -> Result<BookRenderModel> {
    let metadata = build_metadata(epub);
    let toc_entries_raw = collect_toc_entries(epub);
    let (toc_entries, _) = cleanup_toc_entries(toc_entries_raw, NavCleanupMode::Auto);

    let spine_docs = collect_readable_spine_docs(epub);
    let planned_sections = plan_sections(epub, &spine_docs, &toc_entries)?;
    let spine_hrefs: Vec<String> = spine_docs.iter().map(|doc| doc.href_path.clone()).collect();

    let mut content_cache: HashMap<String, ContentDoc> = HashMap::new();
    let mut stylesheets = Vec::new();
    let mut seen_stylesheet_links = HashSet::new();
    let mut seen_inline_styles = HashSet::new();
    let drafts = planned_sections
        .into_iter()
        .map(|planned| {
            build_section_draft(
                epub,
                &spine_hrefs,
                planned,
                &mut content_cache,
                &mut stylesheets,
                &mut seen_stylesheet_links,
                &mut seen_inline_styles,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    let lookup = build_section_lookup(&drafts);
    let sections = drafts
        .into_iter()
        .map(|mut draft| {
            resolve_links_in_blocks(&mut draft.section.blocks, &lookup);
            draft.section
        })
        .collect::<Vec<_>>();

    let mut toc = toc_entries
        .iter()
        .map(|entry| TocEntry {
            title: entry.title_or_fallback(),
            target: unresolved_internal_target(&entry.href_path, entry.fragment.as_deref()),
        })
        .collect::<Vec<_>>();
    resolve_toc_links(&mut toc, &lookup);

    Ok(BookRenderModel {
        metadata,
        toc,
        cover_image: cover_image_resource(epub),
        stylesheets,
        sections,
    })
}

fn build_metadata(epub: &Epub) -> RenderMetadata {
    let epub_metadata = epub.metadata();
    RenderMetadata {
        title: epub_metadata.title().map(|t| t.value().to_string()),
        creator: epub_metadata
            .creators()
            .next()
            .map(|c| c.value().to_string()),
        language: epub_metadata.language().map(|l| l.value().to_string()),
        identifier: epub_metadata.identifier().map(|i| i.value().to_string()),
        publisher: epub_metadata
            .publishers()
            .next()
            .map(|p| p.value().to_string()),
        description: epub_metadata.description().map(|d| d.value().to_string()),
    }
}

fn plan_sections(
    epub: &Epub,
    spine_docs: &[super::ReadableSpineDoc],
    toc_entries: &[TocEntryInfo],
) -> Result<Vec<PlannedSection>> {
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
            let spine_idx = spine_index_by_href
                .get(&doc.href_path)
                .copied()
                .unwrap_or(0);
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

fn build_section_draft(
    epub: &Epub,
    spine_hrefs: &[String],
    planned: PlannedSection,
    content_cache: &mut HashMap<String, ContentDoc>,
    stylesheets: &mut Vec<StylesheetInput>,
    seen_stylesheet_links: &mut HashSet<String>,
    seen_inline_styles: &mut HashSet<String>,
) -> Result<SectionDraft> {
    let mut blocks = Vec::new();
    let mut public_anchors = HashSet::new();
    let mut targets = HashSet::new();
    let mut section_hrefs = Vec::new();

    for spine_idx in planned.spine_start..=planned.spine_end {
        let Some(href) = spine_hrefs.get(spine_idx) else {
            continue;
        };
        let content = load_content(epub, href, content_cache)?;
        collect_stylesheets_from_content(
            epub,
            content,
            href,
            spine_idx,
            stylesheets,
            seen_stylesheet_links,
            seen_inline_styles,
        );
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
        if nodes.is_empty() {
            continue;
        }

        section_hrefs.push(href.clone());
        let body_root = body_node(&content.document).unwrap_or_else(|| content.document.clone());
        let context = LoweringContext {
            root: body_root,
            href: href.clone(),
            spine_index: spine_idx,
        };
        for anchor in collect_anchors_from_nodes(&nodes) {
            public_anchors.insert(anchor.clone());
            targets.insert(format!("{href}#{anchor}"));
        }
        blocks.extend(parse_blocks(epub, &context, &nodes));
    }

    let mut anchors: Vec<String> = public_anchors.into_iter().collect();
    anchors.sort();
    let title = if planned.title.trim().is_empty() {
        None
    } else {
        Some(planned.title)
    };
    let section = Section {
        id: build_section_id(
            &planned.start_href,
            planned.start_fragment.as_deref(),
            planned.end_href.as_deref(),
            planned.end_fragment.as_deref(),
        ),
        title,
        source: SectionSourceRange {
            start_href: planned.start_href,
            start_fragment: planned.start_fragment,
            end_href: planned.end_href,
            end_fragment: planned.end_fragment,
            spine_start: planned.spine_start,
            spine_end: planned.spine_end,
        },
        anchors,
        blocks,
    };

    Ok(SectionDraft {
        section,
        hrefs: section_hrefs,
        targets,
    })
}

fn collect_stylesheets_from_content(
    epub: &Epub,
    content: &ContentDoc,
    base_href: &str,
    spine_index: usize,
    stylesheets: &mut Vec<StylesheetInput>,
    seen_links: &mut HashSet<String>,
    seen_inline: &mut HashSet<String>,
) {
    if let Ok(head) = content.document.select_first("head") {
        let node = head.as_node();
        if let Ok(links) = node.select("link[rel~='stylesheet']") {
            for link in links {
                let attrs = link.attributes.borrow();
                let Some(href) = attrs.get("href") else {
                    continue;
                };
                if is_external(href) {
                    continue;
                }
                let resolved = resolve_href(base_href, href);
                if seen_links.insert(resolved.clone()) {
                    stylesheets.push(StylesheetInput::Linked(resource_ref_from_href(
                        epub,
                        &resolved,
                        ResourceKind::Stylesheet,
                    )));
                }
            }
        }
        if let Ok(style_nodes) = node.select("style") {
            for style_node in style_nodes {
                let text = style_node.text_contents();
                if text.trim().is_empty() {
                    continue;
                }
                let source = source_map_from_node(
                    &content.document,
                    style_node.as_node(),
                    base_href,
                    spine_index,
                );
                let inline_key = format!("{base_href}:{:?}", source.node_path);
                if seen_inline.insert(inline_key) {
                    stylesheets.push(StylesheetInput::Inline {
                        css_text: text,
                        source,
                    });
                }
            }
        }
    }
}

fn parse_blocks(epub: &Epub, context: &LoweringContext, nodes: &[NodeRef]) -> Vec<Block> {
    let mut blocks = Vec::new();
    for node in nodes {
        if let Some(text) = node.as_text() {
            let normalized = normalize_text(&text.borrow());
            if !normalized.trim().is_empty() {
                blocks.push(Block::Paragraph(Paragraph {
                    inlines: vec![Inline::Text(TextRun {
                        text: normalized,
                        source: source_map_from_body(context, node),
                    })],
                    source: source_map_from_body(context, node),
                }));
            }
            continue;
        }
        if let Some(element) = node.as_element() {
            blocks.extend(parse_element_block(
                epub,
                context,
                node,
                element.name.local.as_ref(),
            ));
        }
    }
    blocks
}

fn parse_element_block(
    epub: &Epub,
    context: &LoweringContext,
    node: &NodeRef,
    tag: &str,
) -> Vec<Block> {
    match tag {
        "p" => vec![Block::Paragraph(Paragraph {
            inlines: parse_inlines(epub, context, node),
            source: source_map_from_body(context, node),
        })],
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = tag
                .strip_prefix('h')
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(1)
                .clamp(1, 6);
            vec![Block::Heading(Heading {
                level,
                inlines: parse_inlines(epub, context, node),
                source: source_map_from_body(context, node),
            })]
        }
        "blockquote" => vec![Block::Quote(QuoteBlock {
            blocks: parse_blocks(epub, context, &node.children().collect::<Vec<_>>()),
            source: source_map_from_body(context, node),
        })],
        "ul" | "ol" => {
            let ordered = tag == "ol";
            let items = node
                .children()
                .filter_map(|child| {
                    let child_tag = child.as_element()?.name.local.to_string();
                    if child_tag != "li" {
                        return None;
                    }
                    Some(ListItem {
                        blocks: parse_blocks(epub, context, &child.children().collect::<Vec<_>>()),
                        source: source_map_from_body(context, &child),
                    })
                })
                .collect::<Vec<_>>();
            vec![Block::List(ListBlock {
                ordered,
                items,
                source: source_map_from_body(context, node),
            })]
        }
        "li" => vec![Block::Paragraph(Paragraph {
            inlines: parse_inlines(epub, context, node),
            source: source_map_from_body(context, node),
        })],
        "figure" => parse_figure(epub, context, node),
        "img" => parse_image_block(epub, context, node)
            .map(Block::Image)
            .into_iter()
            .collect(),
        "table" => vec![Block::Table(parse_table(epub, context, node))],
        "hr" => vec![Block::Rule(RuleBlock {
            source: source_map_from_body(context, node),
        })],
        "pre" => vec![Block::Code(CodeBlock {
            text: normalize_code_block_text(&node.text_contents()),
            source: source_map_from_body(context, node),
        })],
        "br" | "script" | "style" => Vec::new(),
        _ => {
            let nested = parse_blocks(epub, context, &node.children().collect::<Vec<_>>());
            if !nested.is_empty() {
                nested
            } else {
                let inlines = parse_inlines(epub, context, node);
                if inlines.is_empty() {
                    Vec::new()
                } else {
                    vec![Block::Paragraph(Paragraph {
                        inlines,
                        source: source_map_from_body(context, node),
                    })]
                }
            }
        }
    }
}

fn parse_figure(epub: &Epub, context: &LoweringContext, node: &NodeRef) -> Vec<Block> {
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
        let inlines = parse_inlines(epub, context, node);
        if inlines.is_empty() {
            return Vec::new();
        }
        return vec![Block::Paragraph(Paragraph {
            inlines,
            source: source_map_from_body(context, node),
        })];
    };
    parse_image_block(epub, context, &image_node)
        .map(|mut block| {
            block.caption = caption;
            Block::Image(block)
        })
        .into_iter()
        .collect()
}

fn parse_image_block(epub: &Epub, context: &LoweringContext, node: &NodeRef) -> Option<ImageBlock> {
    let element = node.as_element()?;
    let attrs = element.attributes.borrow();
    let src = attrs.get("src")?.trim();
    if src.is_empty() {
        return None;
    }
    Some(ImageBlock {
        resource: resource_ref_for_uri(epub, &context.href, src, ResourceKind::Image),
        alt: attrs
            .get("alt")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        caption: None,
        source: source_map_from_body(context, node),
    })
}

fn parse_table(epub: &Epub, context: &LoweringContext, node: &NodeRef) -> TableBlock {
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
                cells.push(TableCell {
                    is_header: tag == "th",
                    inlines: parse_inlines(epub, context, &child),
                    source: source_map_from_body(context, &child),
                });
            }
            if !cells.is_empty() {
                rows.push(TableRow {
                    cells,
                    source: source_map_from_body(context, row.as_node()),
                });
            }
        }
    }
    TableBlock {
        rows,
        source: source_map_from_body(context, node),
    }
}

fn parse_inlines(epub: &Epub, context: &LoweringContext, node: &NodeRef) -> Vec<Inline> {
    let mut inlines = Vec::new();
    for child in node.children() {
        if let Some(text) = child.as_text() {
            let normalized = normalize_text(&text.borrow());
            if !normalized.trim().is_empty() {
                inlines.push(Inline::Text(TextRun {
                    text: normalized,
                    source: source_map_from_body(context, &child),
                }));
            }
            continue;
        }
        let Some(element) = child.as_element() else {
            continue;
        };
        match element.name.local.as_ref() {
            "br" => inlines.push(Inline::Break(source_map_from_body(context, &child))),
            "em" | "i" | "strong" | "b" | "sup" | "sub" | "code" | "span" => {
                let children = parse_inlines(epub, context, &child);
                let styles = parse_style_hints(
                    element.name.local.as_ref(),
                    element.attributes.borrow().get("style"),
                );
                if styles.is_empty() {
                    inlines.extend(children);
                } else {
                    inlines.push(Inline::Span(Span {
                        children,
                        styles,
                        source: source_map_from_body(context, &child),
                    }));
                }
            }
            "a" => {
                let attrs = element.attributes.borrow();
                let Some(href) = attrs
                    .get("href")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    inlines.extend(parse_inlines(epub, context, &child));
                    continue;
                };
                inlines.push(Inline::Link(LinkSpan {
                    target: resolve_raw_link_target(&context.href, href),
                    children: parse_inlines(epub, context, &child),
                    source: source_map_from_body(context, &child),
                }));
            }
            "img" => {
                let attrs = element.attributes.borrow();
                let Some(src) = attrs
                    .get("src")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                inlines.push(Inline::Image(ImageInline {
                    resource: resource_ref_for_uri(epub, &context.href, src, ResourceKind::Image),
                    alt: attrs
                        .get("alt")
                        .map(|value| normalize_space(value))
                        .filter(|value| !value.is_empty()),
                    source: source_map_from_body(context, &child),
                }));
            }
            "script" | "style" => {}
            _ => inlines.extend(parse_inlines(epub, context, &child)),
        }
    }
    inlines
}

fn parse_style_hints(tag: &str, style: Option<&str>) -> Vec<TextStyleHint> {
    let mut hints = Vec::new();
    match tag {
        "em" | "i" => hints.push(TextStyleHint::Italic),
        "strong" | "b" => hints.push(TextStyleHint::Bold),
        "sup" => hints.push(TextStyleHint::Superscript),
        "sub" => hints.push(TextStyleHint::Subscript),
        "code" => hints.push(TextStyleHint::Code),
        _ => {}
    }

    if let Some(style) = style {
        let normalized = style.to_lowercase();
        if normalized.contains("font-style: italic") {
            hints.push(TextStyleHint::Italic);
        }
        if normalized.contains("font-weight: bold") || normalized.contains("font-weight: 700") {
            hints.push(TextStyleHint::Bold);
        }
        if normalized.contains("text-decoration: underline") {
            hints.push(TextStyleHint::Underline);
        }
    }

    dedupe_style_hints(hints)
}

fn dedupe_style_hints(mut hints: Vec<TextStyleHint>) -> Vec<TextStyleHint> {
    let mut seen = HashSet::new();
    hints.retain(|hint| seen.insert(hint.clone()));
    hints
}

fn source_map_from_body(context: &LoweringContext, node: &NodeRef) -> SourceMap {
    SourceMap {
        spine_index: context.spine_index,
        href: context.href.clone(),
        fragment: nearest_fragment(node, &context.root),
        node_path: node_path_from_root(&context.root, node),
    }
}

fn source_map_from_node(
    root: &NodeRef,
    node: &NodeRef,
    href: &str,
    spine_index: usize,
) -> SourceMap {
    SourceMap {
        spine_index,
        href: href.to_string(),
        fragment: nearest_fragment(node, root),
        node_path: node_path_from_root(root, node),
    }
}

fn build_section_lookup(drafts: &[SectionDraft]) -> HashMap<String, (String, usize)> {
    let mut lookup = HashMap::new();
    for (section_index, draft) in drafts.iter().enumerate() {
        for href in &draft.hrefs {
            lookup
                .entry(href.clone())
                .or_insert((draft.section.id.clone(), section_index));
        }
        for target in &draft.targets {
            lookup
                .entry(target.clone())
                .or_insert((draft.section.id.clone(), section_index));
        }
    }
    lookup
}

fn resolve_toc_links(toc: &mut [TocEntry], lookup: &HashMap<String, (String, usize)>) {
    for entry in toc {
        resolve_link_target(&mut entry.target, lookup);
    }
}

fn resolve_links_in_blocks(blocks: &mut [Block], lookup: &HashMap<String, (String, usize)>) {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => resolve_links_in_inlines(&mut paragraph.inlines, lookup),
            Block::Heading(heading) => resolve_links_in_inlines(&mut heading.inlines, lookup),
            Block::Quote(quote) => resolve_links_in_blocks(&mut quote.blocks, lookup),
            Block::List(list) => {
                for item in &mut list.items {
                    resolve_links_in_blocks(&mut item.blocks, lookup);
                }
            }
            Block::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        resolve_links_in_inlines(&mut cell.inlines, lookup);
                    }
                }
            }
            Block::Image(_) | Block::Rule(_) | Block::Code(_) => {}
        }
    }
}

fn resolve_links_in_inlines(inlines: &mut [Inline], lookup: &HashMap<String, (String, usize)>) {
    for inline in inlines {
        match inline {
            Inline::Span(span) => resolve_links_in_inlines(&mut span.children, lookup),
            Inline::Link(link) => {
                resolve_link_target(&mut link.target, lookup);
                resolve_links_in_inlines(&mut link.children, lookup);
            }
            Inline::Text(_) | Inline::Image(_) | Inline::Break(_) => {}
        }
    }
}

fn resolve_link_target(target: &mut LinkTarget, lookup: &HashMap<String, (String, usize)>) {
    let LinkTarget::Unresolved { href } = target else {
        return;
    };
    let lookup_key = href.clone();
    let Some((section_id, section_index)) = lookup.get(&lookup_key).cloned() else {
        return;
    };
    let (path, fragment) = split_target(&lookup_key);
    *target = LinkTarget::Internal {
        section_id,
        section_index,
        href: path.to_string(),
        fragment: fragment.map(str::to_string),
    };
}

fn resolve_raw_link_target(base_href: &str, raw_href: &str) -> LinkTarget {
    let trimmed = raw_href.trim();
    if is_external(trimmed) {
        return LinkTarget::External {
            href: trimmed.to_string(),
        };
    }
    let (raw_path, raw_fragment) = split_target(trimmed);
    let resolved_path = if raw_path.is_empty() {
        normalize_path(base_href)
    } else {
        resolve_href(base_href, raw_path)
    };
    LinkTarget::Unresolved {
        href: unresolved_target_string(&resolved_path, raw_fragment),
    }
}

fn unresolved_internal_target(href_path: &str, fragment: Option<&str>) -> LinkTarget {
    LinkTarget::Unresolved {
        href: unresolved_target_string(href_path, fragment),
    }
}

fn unresolved_target_string(href_path: &str, fragment: Option<&str>) -> String {
    match fragment.filter(|value| !value.is_empty()) {
        Some(fragment) => format!("{href_path}#{fragment}"),
        None => href_path.to_string(),
    }
}

fn split_target(value: &str) -> (&str, Option<&str>) {
    match value.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (value, None),
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

fn resource_ref_for_uri(
    epub: &Epub,
    base_href: &str,
    raw_uri: &str,
    fallback_kind: ResourceKind,
) -> ResourceRef {
    let trimmed = raw_uri.trim();
    if is_external(trimmed) {
        return ResourceRef {
            href: trimmed.to_string(),
            media_type: None,
            kind: fallback_kind,
        };
    }
    let resolved = resolve_href(base_href, trimmed);
    resource_ref_from_href(epub, &resolved, fallback_kind)
}

fn resource_ref_from_href(epub: &Epub, href: &str, fallback_kind: ResourceKind) -> ResourceRef {
    let normalized = normalize_resource_href(href);
    if let Some(entry) = epub
        .manifest()
        .iter()
        .find(|entry| entry.href().as_str() == normalized)
    {
        return ResourceRef {
            href: normalized,
            media_type: Some(entry.media_type().to_string()),
            kind: fallback_kind,
        };
    }
    ResourceRef {
        href: normalized,
        media_type: None,
        kind: fallback_kind,
    }
}

fn cover_image_resource(epub: &Epub) -> Option<ResourceRef> {
    let cover = epub.manifest().cover_image()?;
    Some(ResourceRef {
        href: cover.href().as_str().to_string(),
        media_type: Some(cover.media_type().to_string()),
        kind: ResourceKind::Cover,
    })
}

fn normalize_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_code_block_text(text: &str) -> String {
    text.trim_matches('\n').to_string()
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
    normalize_path(&decoded)
}

trait TocEntryRenderExt {
    fn title_or_fallback(&self) -> String;
}

impl TocEntryRenderExt for TocEntryInfo {
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
    use kuchiki::parse_html;
    use kuchiki::traits::TendrilSink;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/books")
            .join(name)
    }

    fn lowering_context(html: &str, href: &str) -> (NodeRef, LoweringContext) {
        let document = parse_html().one(html);
        let body = body_node(&document).expect("body");
        let context = LoweringContext {
            root: body.clone(),
            href: href.to_string(),
            spine_index: 0,
        };
        (document, context)
    }

    #[test]
    fn parses_sections_and_ir_from_toc() {
        let book = parse_epub_render_model(&fixture("Alice's Adventures in Wonderland.epub"))
            .expect("parse alice");
        assert!(!book.sections.is_empty());
        assert!(!book.toc.is_empty());
        assert!(!book.sections[0].blocks.is_empty());
        assert!(book.sections.iter().any(|section| {
            section
                .title
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains("chapter")
        }));
    }

    #[test]
    fn supports_fragment_slicing_and_heading_fallback() {
        let book = parse_epub_render_model(
            &fixture("Marcus Aurelius, Gregory Hays - Meditations_ A New Translation-Modern Library (2003).epub"),
        )
        .expect("parse meditations");
        assert!(book.sections.len() > 3);
        assert!(
            book.sections
                .iter()
                .all(|section| section.title.as_deref().unwrap_or("").trim().len() > 0)
        );
    }

    #[test]
    fn reads_image_resources() {
        let book = parse_epub_render_model(&fixture("Alice's Adventures in Wonderland.epub"))
            .expect("parse alice");
        let image_resource = book
            .sections
            .iter()
            .flat_map(|section| section.blocks.iter())
            .find_map(|block| match block {
                Block::Image(image) => Some(image.resource.clone()),
                _ => None,
            })
            .expect("image block");
        let bytes = read_epub_resource_bytes(
            &fixture("Alice's Adventures in Wonderland.epub"),
            &image_resource,
        )
        .expect("read resource");
        assert!(bytes.is_some());
        assert!(!bytes.unwrap().is_empty());
    }

    #[test]
    fn collects_stylesheets_and_cover_as_resources() {
        let book = parse_epub_render_model(&fixture("Alice's Adventures in Wonderland.epub"))
            .expect("parse alice");
        assert!(!book.stylesheets.is_empty());
        if let Some(cover) = &book.cover_image {
            assert_eq!(cover.kind, ResourceKind::Cover);
        }
    }

    #[test]
    fn lowers_inline_images_and_code_spans() {
        let (document, context) = lowering_context(
            "<html><body><p>Hello <img src=\"images/a.png\" alt=\"A\"> <code>x</code></p></body></html>",
            "OPS/chapter.xhtml",
        );
        let body = body_node(&document).expect("body");
        let nodes: Vec<NodeRef> = body.children().collect();
        let epub =
            Epub::open(&fixture("Alice's Adventures in Wonderland.epub")).expect("open alice");
        let blocks = parse_blocks(&epub, &context, &nodes);
        let Block::Paragraph(paragraph) = &blocks[0] else {
            panic!("paragraph");
        };
        assert!(
            paragraph
                .inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Image(_)))
        );
        assert!(paragraph.inlines.iter().any(|inline| match inline {
            Inline::Span(span) => span.styles.contains(&TextStyleHint::Code),
            _ => false,
        }));
    }

    #[test]
    fn lowers_figure_images_and_code_blocks() {
        let (document, context) = lowering_context(
            "<html><body><figure><img src=\"images/a.png\" alt=\"A\"><figcaption>Caption</figcaption></figure><pre><code>fn main() {\n    println!(\"hi\");\n}</code></pre></body></html>",
            "OPS/chapter.xhtml",
        );
        let body = body_node(&document).expect("body");
        let nodes: Vec<NodeRef> = body.children().collect();
        let epub =
            Epub::open(&fixture("Alice's Adventures in Wonderland.epub")).expect("open alice");
        let blocks = parse_blocks(&epub, &context, &nodes);
        assert!(
            matches!(&blocks[0], Block::Image(image) if image.caption.as_deref() == Some("Caption"))
        );
        assert!(matches!(&blocks[1], Block::Code(code) if code.text.contains("println!")));
    }

    #[test]
    fn resolves_internal_links_and_keeps_source_maps() {
        let (document, context) = lowering_context(
            "<html><body><p><a href=\"#frag\">Jump</a> <a href=\"https://example.com\">Out</a></p><h2 id=\"frag\">Target</h2></body></html>",
            "OPS/chapter.xhtml",
        );
        let body = body_node(&document).expect("body");
        let nodes: Vec<NodeRef> = body.children().collect();
        let epub =
            Epub::open(&fixture("Alice's Adventures in Wonderland.epub")).expect("open alice");
        let mut blocks = parse_blocks(&epub, &context, &nodes);
        let mut lookup = HashMap::new();
        lookup.insert(
            "OPS/chapter.xhtml#frag".to_string(),
            ("section-1".to_string(), 0usize),
        );
        resolve_links_in_blocks(&mut blocks, &lookup);
        let Block::Paragraph(paragraph) = &blocks[0] else {
            panic!("paragraph");
        };
        assert!(paragraph.source.href.ends_with("chapter.xhtml"));
        assert!(paragraph.inlines.iter().any(|inline| match inline {
            Inline::Link(LinkSpan {
                target:
                    LinkTarget::Internal {
                        section_id,
                        section_index,
                        href,
                        fragment,
                    },
                ..
            }) => {
                section_id == "section-1"
                    && *section_index == 0
                    && href == "OPS/chapter.xhtml"
                    && fragment.as_deref() == Some("frag")
            }
            _ => false,
        }));
        assert!(paragraph.inlines.iter().any(|inline| match inline {
            Inline::Link(LinkSpan {
                target: LinkTarget::External { href },
                ..
            }) => href == "https://example.com",
            _ => false,
        }));
    }

    #[test]
    fn parsed_books_keep_source_maps_on_emitted_nodes() {
        let book = parse_epub_render_model(&fixture("Alice's Adventures in Wonderland.epub"))
            .expect("parse alice");
        let mut saw_source = false;
        for section in &book.sections {
            for block in &section.blocks {
                match block {
                    Block::Paragraph(paragraph) => {
                        saw_source |= !paragraph.source.href.is_empty();
                        for inline in &paragraph.inlines {
                            if let Inline::Link(link) = inline {
                                saw_source |= !link.source.href.is_empty();
                            }
                        }
                    }
                    Block::Heading(heading) => {
                        saw_source |= !heading.source.href.is_empty();
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_source);
    }
}
