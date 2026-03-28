use anyhow::{Context, Result};
use kuchiki::parse_html;
use kuchiki::traits::*;
use rbook::Epub;
use std::collections::HashMap;

use super::{ContentDoc, ReadableSpineDoc, TocEntryInfo};

const READABLE_MIME: &[&str] = &["application/xhtml+xml", "text/html"];

pub(super) fn collect_image_hrefs(epub: &Epub) -> Vec<String> {
    epub.manifest()
        .images()
        .map(|entry| entry.href().as_str().to_string())
        .collect()
}

pub(super) fn collect_media_hrefs(epub: &Epub) -> Vec<String> {
    epub.manifest()
        .iter()
        .filter(|entry| {
            let kind = entry.kind();
            kind.is_audio() || kind.is_video()
        })
        .map(|entry| entry.href().as_str().to_string())
        .collect()
}

pub(super) fn collect_readable_spine_docs(epub: &Epub) -> Vec<ReadableSpineDoc> {
    epub.spine()
        .iter()
        .filter_map(|entry| entry.manifest_entry())
        .filter(|entry| is_readable(entry.media_type()))
        .map(|entry| ReadableSpineDoc {
            href_path: entry.href().as_str().to_string(),
            label: entry.href().name().decode().to_string(),
        })
        .collect()
}

pub(super) fn collect_toc_entries(epub: &Epub) -> Vec<TocEntryInfo> {
    let Some(root) = epub.toc().contents() else {
        return Vec::new();
    };

    root.flatten()
        .filter_map(|entry| {
            let href = entry.href()?;
            if let Some(manifest_entry) = entry.manifest_entry() {
                if !is_readable(manifest_entry.media_type()) {
                    return None;
                }
            }
            Some(TocEntryInfo {
                label: entry.label().to_string(),
                href_path: href.path().as_str().to_string(),
                fragment: href.fragment().map(|frag| frag.to_string()),
            })
        })
        .collect()
}

pub(super) fn load_content<'a>(
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
