input := "assets"
output := "results"

rbook-utils-convert-plain:
    cargo run -- --input {{input}} --output {{output}}

rbook-utils-convert-rich:
    cargo run -- --input {{input}} --output {{output}} --markdown-mode rich --style inline --media all --split-chapters
