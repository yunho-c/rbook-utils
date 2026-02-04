use std::path::PathBuf;

use clap::Parser;
use rbook_utils::{ConvertOptions, MarkdownMode, StyleMode, convert_all};

#[derive(Parser, Debug)]
#[command(name = "rbook-utils")]
#[command(about = "EPUB to Markdown conversion powered by rbook")]
struct Cli {
    #[arg(long, default_value = "assets")]
    input_dir: PathBuf,
    #[arg(long, default_value = "rbook-utils/results")]
    output_dir: PathBuf,
    #[arg(long)]
    media_all: bool,
    #[arg(long, value_enum, default_value_t = MarkdownMode::Plain)]
    markdown_mode: MarkdownMode,
    #[arg(long, value_enum, default_value_t = StyleMode::Inline)]
    style: StyleMode,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut options = ConvertOptions::new(cli.input_dir, cli.output_dir);
    options.media_all = cli.media_all;
    options.markdown_mode = cli.markdown_mode;
    options.style = cli.style;

    convert_all(&options)
}
