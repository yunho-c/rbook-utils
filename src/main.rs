use std::path::PathBuf;

use clap::Parser;
use rbook_utils::{
    ChapterFallbackMode, ConvertOptions, CssMode, ExportMode, FilenameScheme, FormatMode,
    MediaMode, NavCleanupMode, NotesMode, OcrCleanupMode, convert_all,
};

#[derive(Parser, Debug)]
#[command(name = "rbook-utils")]
#[command(about = "EPUB to Markdown conversion powered by rbook")]
struct Cli {
    #[arg(long, default_value = "assets")]
    input: PathBuf,
    #[arg(long, default_value = "rbook-utils/results")]
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = MediaMode::Image)]
    media: MediaMode,
    #[arg(long, value_enum, default_value_t = FormatMode::Plain)]
    format: FormatMode,
    #[arg(long, value_enum, default_value_t = CssMode::Inline)]
    css: CssMode,
    #[arg(long)]
    split_chapters: bool,
    #[arg(long, value_enum, default_value_t = ChapterFallbackMode::Auto)]
    chapter_fallback: ChapterFallbackMode,
    #[arg(long, value_enum, default_value_t = NotesMode::Inline)]
    notes_mode: NotesMode,
    #[arg(long, value_enum, default_value_t = ExportMode::Off)]
    export_manifest: ExportMode,
    #[arg(long, value_enum, default_value_t = ExportMode::Off)]
    quality_report: ExportMode,
    #[arg(long, value_enum, default_value_t = OcrCleanupMode::Off)]
    ocr_cleanup: OcrCleanupMode,
    #[arg(long, value_enum, default_value_t = NavCleanupMode::Auto)]
    nav_cleanup: NavCleanupMode,
    #[arg(long, value_enum, default_value_t = FilenameScheme::Index)]
    filename_scheme: FilenameScheme,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut options = ConvertOptions::new(cli.input, cli.output);
    options.media = cli.media;
    options.format = cli.format;
    options.css = cli.css;
    options.split_chapters = cli.split_chapters;
    options.chapter_fallback = cli.chapter_fallback;
    options.notes_mode = cli.notes_mode;
    options.export_manifest = cli.export_manifest;
    options.quality_report = cli.quality_report;
    options.ocr_cleanup = cli.ocr_cleanup;
    options.nav_cleanup = cli.nav_cleanup;
    options.filename_scheme = cli.filename_scheme;

    let summary = convert_all(&options)?;
    let mut failures = 0usize;
    for book in &summary.books {
        let mut has_error = false;
        for diagnostic in &book.diagnostics {
            match diagnostic.level {
                rbook_utils::DiagnosticLevel::Info => {
                    println!("{}", diagnostic.message);
                }
                rbook_utils::DiagnosticLevel::Warning => {
                    eprintln!("Warning: {}", diagnostic.message);
                }
                rbook_utils::DiagnosticLevel::Error => {
                    has_error = true;
                    eprintln!("Error: {}", diagnostic.message);
                }
            }
        }

        if let Some(path) = &book.output_path {
            if options.split_chapters {
                println!("Wrote chapter files to {}", path.display());
            } else {
                println!("Wrote {}", path.display());
            }
        } else {
            has_error = true;
        }

        if has_error {
            failures += 1;
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} EPUB(s) failed to parse");
    }

    Ok(())
}
