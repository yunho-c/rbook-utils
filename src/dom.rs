use kuchiki::NodeRef;
use std::collections::HashSet;

use super::{ContentDoc, is_external, resolve_href};

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

pub(super) fn slice_content_nodes(
    content: &ContentDoc,
    start_fragment: Option<&str>,
    end_fragment: Option<&str>,
) -> Vec<NodeRef> {
    let Some(body) = body_node(&content.document) else {
        return Vec::new();
    };
    let children: Vec<NodeRef> = body.children().collect();
    if children.is_empty() {
        return Vec::new();
    }
    if start_fragment.is_none() && end_fragment.is_none() {
        return children;
    }

    let mut start_idx = 0usize;
    if let Some(fragment) = start_fragment {
        let Some(anchor) = find_anchor(&content.document, fragment) else {
            return Vec::new();
        };
        let Some(top) = top_level_body_child(&body, &anchor) else {
            return Vec::new();
        };
        let Some(idx) = child_index(&children, &top) else {
            return Vec::new();
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
        return Vec::new();
    }
    children[start_idx..end_idx].to_vec()
}

pub(super) fn collect_anchors_from_nodes(nodes: &[NodeRef]) -> Vec<String> {
    let mut anchors: HashSet<String> = HashSet::new();
    for node in nodes {
        if let Some(id) = node_fragment(node) {
            anchors.insert(id);
        }
        if let Ok(matches) = node.select("[id]") {
            for n in matches {
                if let Some(id) = node_fragment(n.as_node()) {
                    anchors.insert(id);
                }
            }
        }
        if let Ok(matches) = node.select("a[name]") {
            for n in matches {
                if let Some(id) = node_fragment(n.as_node()) {
                    anchors.insert(id);
                }
            }
        }
    }
    let mut values: Vec<String> = anchors.into_iter().collect();
    values.sort();
    values
}

pub(super) fn body_node(document: &NodeRef) -> Option<NodeRef> {
    document
        .select_first("body")
        .ok()
        .map(|node| node.as_node().clone())
}

pub(super) fn node_path_from_root(root: &NodeRef, node: &NodeRef) -> Vec<u32> {
    if root == node {
        return Vec::new();
    }

    let mut path = Vec::new();
    let mut current = node.clone();
    loop {
        let Some(parent) = current.parent() else {
            return Vec::new();
        };
        let Some(idx) = parent.children().position(|child| child == current) else {
            return Vec::new();
        };
        path.push(idx as u32);
        if parent == *root {
            path.reverse();
            return path;
        }
        current = parent;
    }
}

pub(super) fn nearest_fragment(node: &NodeRef, root: &NodeRef) -> Option<String> {
    let mut current = Some(node.clone());
    while let Some(candidate) = current {
        if let Some(fragment) = node_fragment(&candidate) {
            return Some(fragment);
        }
        if candidate == *root {
            break;
        }
        current = candidate.parent();
    }
    None
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

fn node_fragment(node: &NodeRef) -> Option<String> {
    let element = node.as_element()?;
    let attrs = element.attributes.borrow();
    attrs
        .get("id")
        .or_else(|| {
            if element.name.local.as_ref() == "a" {
                attrs.get("name")
            } else {
                None
            }
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
