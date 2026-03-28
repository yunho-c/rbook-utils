use regex::Regex;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};

use super::{
    FOOTNOTE_DEF_RE, FilenameScheme, HTML_HREF_RE, MARKDOWN_LINK_RE, NavCleanupMode, NotesMode,
    OCR_NOISE_RE, OcrCleanupMode, PostprocessStats, SectionRecord, TocEntryInfo, normalize_path,
    normalize_space, resolve_href, slugify,
};

pub(super) fn cleanup_toc_entries(
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

pub(super) fn postprocess_sections(
    sections: &mut [SectionRecord],
    split_chapters: bool,
    filename_scheme: FilenameScheme,
    book_slug: &str,
    ocr_cleanup: OcrCleanupMode,
    notes_mode: NotesMode,
) -> PostprocessStats {
    let mut stats = PostprocessStats::default();
    for section in sections.iter_mut() {
        section.section_id = build_section_id(
            &section.start_href,
            section.start_fragment.as_deref(),
            section.end_href.as_deref(),
            section.end_fragment.as_deref(),
        );
        let (cleaned, changes) = apply_ocr_cleanup(&section.text, ocr_cleanup);
        section.text = cleaned;
        stats.cleanup_changes += changes;
    }
    assign_section_output_paths(sections, split_chapters, filename_scheme, book_slug);
    let (rewritten, unresolved) = rewrite_section_links(sections, split_chapters);
    stats.link_rewritten = rewritten;
    stats.link_unresolved = unresolved;
    let (notes_written, global_note_lines) = apply_notes_mode_to_sections(sections, notes_mode);
    stats.notes_written = notes_written;
    stats.global_note_lines = global_note_lines;
    stats
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

fn assign_section_output_paths(
    sections: &mut [SectionRecord],
    split_chapters: bool,
    filename_scheme: FilenameScheme,
    book_slug: &str,
) {
    if !split_chapters {
        for section in sections {
            section.output_path = format!("{book_slug}.md");
        }
        return;
    }
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
        section.output_path = match filename_scheme {
            FilenameScheme::Index => {
                format!("{:0width$}_{}.md", idx + 1, section_slug, width = width)
            }
            FilenameScheme::Hash => format!("{}_{}.md", section.section_id, section_slug),
        };
    }
}

fn rewrite_section_links(sections: &mut [SectionRecord], split_chapters: bool) -> (usize, usize) {
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
            if split_chapters {
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
    (link_rewritten, link_unresolved)
}

fn apply_notes_mode_to_sections(
    sections: &mut [SectionRecord],
    notes_mode: NotesMode,
) -> (usize, Vec<String>) {
    if notes_mode == NotesMode::Inline {
        return (0, Vec::new());
    }
    let mut notes_written = 0usize;
    let mut global_note_lines: Vec<String> = Vec::new();
    for section in sections {
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
        match notes_mode {
            NotesMode::Inline => {}
            NotesMode::ChapterEnd => {
                section.text = format!(
                    "{}\n\n### Notes\n\n{}",
                    section.text.trim(),
                    rendered_defs.join("\n")
                );
            }
            NotesMode::Global => {
                global_note_lines.push(format!("## {} ({})", section.title, section.section_id));
                global_note_lines.push(String::new());
                global_note_lines.extend(rendered_defs);
                global_note_lines.push(String::new());
            }
        }
    }
    (notes_written, global_note_lines)
}
