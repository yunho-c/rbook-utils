# Render IR on Top of `rbook`

## Overview

`rbook` is a strong fit for the ebook package and navigation layer of an app:

- container/archive access
- metadata
- manifest/resource lookup
- spine order
- table of contents
- chapter/resource text retrieval

It is not, however, a renderer-oriented document IR.

Today, the readable body-content surface is effectively raw XHTML/HTML text plus
EPUB context. That is useful for ingestion, but a text renderer usually wants a
normalized structure that is stable across formats and independent from package
details.

The recommended architecture is:

1. Use `rbook` to open and inspect the ebook.
2. Read spine resources as XHTML/HTML strings.
3. Parse those resources into a DOM-like intermediate form.
4. Lower that tree into an app-owned render IR.
5. Run styling, layout, pagination, selection, and painting on that IR.

In short: `rbook` should sit below the render pipeline, not be the render
pipeline.

## What `rbook` Already Gives You

`rbook` is already a good source of truth for:

- book metadata
- reading order
- navigation
- resource addressing
- image and stylesheet discovery
- internal link resolution inputs

That means the renderer does not need to understand EPUB packaging directly.
It only needs a clean content model derived from the resources that `rbook`
loads.

## What a Renderer Usually Needs

A renderer-oriented IR should be:

- semantic
- normalized
- layout-friendly
- stable under theme/style changes
- independent from raw XHTML quirks
- easy to map back to source positions

Raw XHTML is usually the wrong long-term input for:

- pagination
- text selection
- search highlighting
- annotation anchoring
- accessibility ranges
- TTS synchronization
- view diffing

## Recommended Layering

Use these layers:

1. `rbook` model layer
2. XHTML parser layer
3. lowering layer
4. style resolution layer
5. layout/pagination layer
6. render layer

The key rule is that layout code should operate on your app IR, not on raw XML
or EPUB-specific types.

## Proposed IR

This is a reasonable starting point for a renderer-facing IR:

```rust
#[derive(Debug, Clone)]
pub struct BookRenderModel {
    pub sections: Vec<Section>,
    pub stylesheets: Vec<StylesheetRef>,
    pub metadata: RenderMetadata,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub id: String,
    pub href: String,
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Paragraph(Paragraph),
    Heading(Heading),
    Quote(Vec<Block>),
    List(ListBlock),
    Table(TableBlock),
    Image(ImageBlock),
    Rule,
    Code(CodeBlock),
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub inlines: Vec<Inline>,
    pub style: BlockStyle,
}

#[derive(Debug, Clone)]
pub struct Heading {
    pub level: u8,
    pub inlines: Vec<Inline>,
    pub style: BlockStyle,
}

#[derive(Debug, Clone)]
pub enum Inline {
    Text(TextRun),
    Span(Span),
    Link(LinkSpan),
    Image(ImageInline),
    Break,
}

#[derive(Debug, Clone)]
pub struct TextRun {
    pub text: String,
    pub style: TextStyle,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub children: Vec<Inline>,
    pub style: TextStyle,
}

#[derive(Debug, Clone)]
pub struct LinkSpan {
    pub href: String,
    pub children: Vec<Inline>,
    pub internal_target: Option<NavTarget>,
}

#[derive(Debug, Clone)]
pub struct NavTarget {
    pub section_index: usize,
    pub fragment: Option<String>,
}
```

This is intentionally not a DOM clone.

The goal is to preserve rendering semantics while discarding source-format noise.

## Lowering Rules

The lowering pass should be deterministic and boring:

- `p` -> paragraph block
- `h1..h6` -> heading block
- `blockquote` -> quote block
- `ul` and `ol` -> list block
- `li` -> list item nodes inside a list block
- `img` -> block image or inline image depending on context
- `a` -> inline link
- `br` -> inline break
- `em`, `strong`, `code`, `span` -> inline span with style flags
- unknown inline-ish elements -> flatten children
- unknown block-ish elements -> preserve as generic block or flatten, depending on policy

Do not carry arbitrary XHTML structure into layout unless there is a concrete
rendering need.

## Source Mapping

Add source mapping to the IR early. It will pay for itself later.

```rust
#[derive(Debug, Clone)]
pub struct SourceMap {
    pub spine_index: usize,
    pub href: String,
    pub fragment: Option<String>,
    pub node_path: Vec<u32>,
}
```

This enables:

- reading-position persistence
- internal navigation
- search results
- annotations and highlights
- fragment targeting
- TTS and accessibility synchronization

## How `rbook` Fits Into This

`rbook` should supply:

- spine traversal
- manifest lookup
- raw chapter content
- href and path helpers
- metadata and TOC context

Your app-side adapter should:

1. iterate the spine
2. read each chapter/resource string
3. parse the XHTML
4. lower it into `Section { blocks, ... }`
5. resolve links and assets against the manifest
6. store stylesheet references for later style resolution

That keeps EPUB packaging concerns below the renderer boundary.

## Link and Asset Resolution

Resolve all links and asset references during or immediately after lowering.

For example:

- `href="chapter2.xhtml#frag"` should become a resolved internal target when possible
- image references should be rewritten into manifest/resource handles
- stylesheet references should become explicit stylesheet inputs

The renderer should not need to understand package-relative EPUB paths.

## Styling Strategy

Treat styling as a separate pass.

The lowered IR should carry semantic structure first, then acquire computed
style data derived from:

- default reading styles
- publisher CSS
- user theme overrides
- accessibility overrides

This separation makes theme switching and relayout much easier.

## Non-Goals

Do not make the render IR:

- EPUB-specific
- XML-shaped
- tightly coupled to manifest IDs
- dependent on raw DOM traversal at render time

Do not use raw chapter XHTML as the long-term layout input.

## Practical Recommendation

If building an ebook app on top of `rbook`, use:

- `rbook` as the ingestion and publication model layer
- your own render IR as the document and layout layer

That gives you a cleaner architecture and leaves room for future non-EPUB
formats without rewriting the renderer.

