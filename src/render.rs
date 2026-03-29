use anyhow::Result;
use kuchiki::NodeRef;
use rbook::Epub;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::{
    COMPLEX_HTML_TAGS, ContentDoc, CssMode, FormatMode, decode_path, is_external, resolve_href,
};

pub(super) fn collect_css(
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
        render_nodes_for_mode(nodes, content, format, image_resolver),
        collect_anchors_from_nodes(nodes),
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
