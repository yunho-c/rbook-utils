use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{ConvertOptions, ExportMode, NotesMode, PostprocessStats, SectionRecord, TocEntryInfo};

pub(super) fn write_markdown_outputs(
    sections: &[SectionRecord],
    options: &ConvertOptions,
    output: &Path,
    book_dir: &Path,
    book_slug: &str,
    title: &str,
    author: Option<&String>,
    style_header_lines: &[String],
    global_note_lines: &[String],
) -> Result<std::path::PathBuf> {
    let output_root = if options.split_chapters {
        book_dir.to_path_buf()
    } else {
        output.to_path_buf()
    };
    fs::create_dir_all(&output_root)?;

    let mut base_lines = Vec::new();
    base_lines.push(format!("# {title}"));
    if let Some(author) = author {
        base_lines.push(format!("**Author:** {author}"));
    }
    if !style_header_lines.is_empty() {
        base_lines.push(String::new());
        base_lines.extend(style_header_lines.to_vec());
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
        for section in sections {
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
        for section in sections {
            lines.push(format!("<a id=\"{}\"></a>", section.section_id));
            lines.push(format!("## {}", section.title));
            lines.push(String::new());
            lines.push(section.text.clone());
            lines.push(String::new());
        }
        if options.notes_mode == NotesMode::Global && !global_note_lines.is_empty() {
            lines.push("## Notes".to_string());
            lines.push(String::new());
            lines.extend(global_note_lines.to_vec());
        }
        fs::write(&output_path, lines.join("\n").trim().to_string() + "\n")?;
        return_path = output_path;
    }

    if options.notes_mode == NotesMode::Global && !global_note_lines.is_empty() {
        fs::create_dir_all(book_dir)?;
        fs::write(
            book_dir.join("notes.md"),
            format!("# Notes\n\n{}\n", global_note_lines.join("\n").trim()),
        )?;
    }
    Ok(return_path)
}

pub(super) fn write_manifest_export(
    enabled: ExportMode,
    book_dir: &Path,
    title: &str,
    author: Option<&String>,
    book_slug: &str,
    spine_hrefs: &[String],
    toc_entries: &[TocEntryInfo],
    sections: &[SectionRecord],
    extracted_images: &HashMap<String, String>,
    extracted_media: &HashMap<String, String>,
    options: &ConvertOptions,
) -> Result<()> {
    if enabled != ExportMode::V1 {
        return Ok(());
    }
    fs::create_dir_all(book_dir)?;
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
            "authors": author.cloned().unwrap_or_default(),
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
            "format": format!("{:?}", options.format),
            "css": format!("{:?}", options.css),
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
    Ok(())
}

pub(super) fn write_quality_report(
    enabled: ExportMode,
    book_dir: &Path,
    toc_entry_count: usize,
    toc_unique_count: usize,
    toc_coverage_ratio: f32,
    toc_is_degenerate: bool,
    use_heading_fallback: bool,
    options: &ConvertOptions,
    stats: &PostprocessStats,
    extracted_count: usize,
    extracted_media_count: usize,
    nav_removed: usize,
    warnings: &[String],
    errors: &[String],
) -> Result<()> {
    if enabled != ExportMode::V1 {
        return Ok(());
    }
    fs::create_dir_all(book_dir)?;
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
            "rewritten": stats.link_rewritten,
            "unresolved": stats.link_unresolved,
        },
        "asset_stats": {
            "images_extracted": extracted_count,
            "media_extracted": extracted_media_count,
            "missing_assets": warnings.iter().filter(|msg| msg.contains("missing media")).count(),
        },
        "ocr_stats": {
            "mode": format!("{:?}", options.ocr_cleanup),
            "cleanup_changes": stats.cleanup_changes,
        },
        "cleanup_stats": {
            "nav_cleanup_mode": format!("{:?}", options.nav_cleanup),
            "toc_entries_removed": nav_removed,
        },
        "notes_stats": {
            "mode": format!("{:?}", options.notes_mode),
            "notes_written": stats.notes_written,
        },
        "warnings": warnings,
        "errors": errors,
    });
    fs::write(
        book_dir.join("report.v1.json"),
        serde_json::to_string_pretty(&report)? + "\n",
    )?;
    Ok(())
}
