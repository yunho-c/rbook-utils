use rbook::Epub;
use std::collections::HashMap;

use super::collect::load_content;
use super::{
    ContentDoc, HeadingCandidate, MAJOR_HEADING_RE, OCR_NOISE_RE, clean_heading_label,
    extract_major_heading_label, is_heading_like_line, normalize_space,
};

pub(super) fn detect_heading_candidates(
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

pub(super) fn prettify_section_name(value: &str) -> String {
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
