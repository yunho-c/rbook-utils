manifest := "rbook-utils/Cargo.toml"
input_dir := "assets"
output_dir := "rbook-utils/results"

rbook-utils-convert-plain:
    cargo run --manifest-path {{manifest}} -- --input-dir {{input_dir}} --output-dir {{output_dir}}

rbook-utils-convert-rich:
    cargo run --manifest-path {{manifest}} -- --input-dir {{input_dir}} --output-dir {{output_dir}} --markdown-mode rich --style inline --media-all --split-chapters
