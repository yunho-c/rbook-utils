use anyhow::Result;
use kuchiki::NodeRef;
use rbook::Epub;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::dom::{collect_anchors_from_nodes, slice_content_nodes};
use super::{
    COMPLEX_HTML_TAGS, ContentDoc, CssMode, FormatMode, decode_path, is_external, resolve_href,
};

pub(super) fn build_style_header(
    epub: &Epub,
    css_hrefs: &HashSet<String>,
    inline_styles: &[String],
    styles_root: &Path,
    style_link_prefix: &str,
    css_mode: CssMode,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    if css_hrefs.is_empty() && inline_styles.is_empty() {
        return Ok(lines);
    }

    match css_mode {
        CssMode::External => {
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
        CssMode::Inline => {
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

pub(super) fn render_partial_with_anchors(
    content: &ContentDoc,
    format: FormatMode,
    start_fragment: Option<&str>,
    end_fragment: Option<&str>,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> (Option<String>, Vec<String>) {
    if start_fragment.is_none() && end_fragment.is_none() {
        return (
            render_full_content(content, format, image_resolver),
            collect_anchors_from_content(content),
        );
    }
    let nodes = slice_content_nodes(content, start_fragment, end_fragment);
    if nodes.is_empty() {
        return (None, Vec::new());
    }
    (
        render_nodes_for_mode(&nodes, content, format, image_resolver),
        collect_anchors_from_nodes(&nodes),
    )
}

pub(super) fn resolve_and_extract_image(
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

pub(super) fn extract_image(
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

pub(super) fn extract_media_file(
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

fn render_full_content(
    content: &ContentDoc,
    format: FormatMode,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    if let Ok(body) = content.document.select_first("body") {
        let body = body.as_node().clone();
        match format {
            FormatMode::Plain => render_plain(&body, content, image_resolver),
            FormatMode::Rich => Some(render_rich(&body, content, image_resolver)),
        }
    } else {
        None
    }
}

fn collect_anchors_from_content(content: &ContentDoc) -> Vec<String> {
    collect_anchors_from_nodes(&slice_content_nodes(content, None, None))
}

fn render_nodes_for_mode(
    nodes: &[NodeRef],
    content: &ContentDoc,
    format: FormatMode,
    image_resolver: &mut impl FnMut(&str, &str) -> Option<String>,
) -> Option<String> {
    match format {
        FormatMode::Plain => render_nodes_plain(nodes, content, image_resolver),
        FormatMode::Rich => {
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
