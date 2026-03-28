# EPUB Parsing Gold Standard

This document defines the complete parsing process used in this repository, independent of language/runtime. It is meant to be the source of truth for reproducing the same quality in another platform (for example, a mobile app).

## 1) Goals

- Produce stable, readable Markdown from EPUBs.
- Preserve document structure (TOC chapters, headings, tables/figures where needed).
- Extract and relink local assets (images, optional CSS).
- Handle real-world EPUB quirks (split title/content files, weak TOCs, inconsistent hrefs).
- Keep output deterministic across repeated runs.

## 2) Inputs and outputs

Inputs:

- EPUB files from an input directory.
- Flags controlling conversion behavior.

Outputs (per book):

- Markdown in `results/<book_slug>.md` (single-file mode), or
- Chapter files in `results/<book_slug>/<NN>_<chapter_slug>.md` (split mode).
- Extracted images in `results/<book_slug>/images/...`.
- Extracted styles (external mode) in `results/<book_slug>/styles/...`.

## 3) Canonical pipeline

### Stage A: Open EPUB and build indexes

1. Open EPUB container/package.
2. Read metadata (title, creators/authors).
3. Build manifest indexes:
   - by `id`
   - by normalized `href`
4. Build spine list using readable media types only:
   - `application/xhtml+xml`
   - `text/html`
5. Build TOC entries (`label`, `href`, optional `fragment`).
6. Build `spine_index_by_href` for ordering and boundary math.

Normalization rules used throughout:

- Convert backslashes to `/`.
- Drop query string (`?foo=bar`) for resource matching.
- Normalize dot segments (`./`, `../`) using posix semantics.
- Compare with normalized paths only.

### Stage B: Convert TOC into chapter boundaries

This is the key algorithm and the reason Meditations-style books work correctly.

For TOC entry `i`:

- `start = (toc[i].href, toc[i].fragment)`
- `end = (toc[i+1].href, toc[i+1].fragment)` if next entry exists, else EOF

Then render all spine files from `start.href` through `end.href`:

- First file: start at `start.fragment` if present, else from file start.
- Middle files: include whole body.
- Last file: end at `end.fragment` if present.
- Special case: if `end.fragment` is missing, next section starts at beginning of `end.href`, so exclude that file from current section.

Why this works:

- Many EPUBs have TOC points to title-page fragments in one file and actual chapter text in the next file.
- Boundary slicing over spine order naturally stitches these split files.

If TOC is unavailable:

- Fallback to spine order, one section per spine document.

Optional heading fallback (`--chapter-fallback auto|force`):

- Used for low-quality EPUBs where TOC is degenerate.
- In `auto`, fallback is attempted only when:
  - `toc_entry_count <= 1`, OR
  - `unique_toc_hrefs < 3`, OR
  - `toc_coverage_ratio < 0.15`.
- In `force`, fallback is always attempted first.

## 4) Partial slicing model inside an XHTML document

When a fragment is involved:

1. Parse document to DOM.
2. Find anchor by `id`, fallback to `a[name=...]`.
3. Find top-level body child that contains the anchor.
4. Slice `body` top-level children by index:
   - start index from start anchor
   - end index from end anchor (exclusive)
5. Render only that node span.

This avoids brittle text-based slicing and keeps structural integrity.

## 5) Markdown rendering modes

### `plain` mode

- Convert HTML to Markdown text.
- Ignore non-content tags:
  - `head`, `title`, `style`, `script`, `svg`
- Convert `<img>` to Markdown image syntax.

### `rich` mode

- Preserve complex HTML blocks as raw HTML when fidelity matters:
  - tables (`table`, `thead`, `tbody`, `tr`, `td`, `th`)
  - figure-related tags
  - svg/math
  - nodes with `class` or `style` attributes
- Convert simpler blocks to Markdown.
- Rewrite image paths inside preserved HTML too.

## 6) Image/media extraction

For every image reference encountered during conversion:

1. Read `src`.
2. If external (`http`, `https`, `data:`), keep as-is.
3. Else resolve relative to current content href.
4. Validate against manifest or zip entries.
5. Extract bytes to:
   - `results/<book_slug>/images/<resolved_href>`
6. Rewrite Markdown/HTML reference to:
   - `./<book_slug>/images/<resolved_href>`
7. Deduplicate with an in-memory `extracted[href]` map.

`--media-all` behavior:

- In addition to referenced images, extract all manifest images (`media_type` starts with `image/`).

## 7) CSS handling (rich mode only)

Collect from each processed XHTML:

- `<link rel="stylesheet" href="...">` (local only)
- `<style>...</style>`

Two styles:

- `--style inline`: emit one `<style>` block in Markdown header with merged CSS.
- `--style external`: write CSS files to `results/<book_slug>/styles/...` and emit `<link>` tags.

Notes:

- Markdown renderers vary widely; many sanitize or ignore `<style>`/`<link>`.
- Rich output is best for controlled renderers (app/webview), not generic Markdown hosts.

## 8) Output assembly

Book-level header:

- `# <title>`
- `**Author:** ...` when available
- style header lines (if rich mode with CSS)

Then sections:

- `## <toc label>`
- section body text/html

Split mode (`--split-chapters`):

- Write one chapter file per section:
  - `index`: `<NN>_<section_slug>.md`
  - `hash`: `<section_id>_<section_slug>.md`
- Place under `results/<book_slug>/`.
- Before writing, remove stale `*.md` in that folder to avoid old/new mixed outputs.

Post-processing:

- Internal link rewrite/validation for split and single-file output.
- Optional note handling:
  - `--notes-mode inline|chapter-end|global`
- Optional JSON exports:
  - `manifest.v1.json` via `--export-manifest v1`
  - `report.v1.json` via `--quality-report v1`
- Optional cleanup:
  - `--nav-cleanup off|auto`
  - `--ocr-cleanup off|basic|aggressive`

## 9) Determinism and safety behaviors

- Stable ordering from spine + TOC flattening.
- Path normalization for consistent matching.
- Best-effort extraction:
  - if image extraction fails, keep original `src` so content is not lost.
- Keep conversion running per book; report failures without crashing whole batch.

## 10) Current CLI surface (gold standard behavior)

Common controls used by both implementations:

- `--input-dir`
- `--output-dir`
- `--media-all`
- `--markdown-mode plain|rich`
- `--style inline|external`
- `--split-chapters`
- `--chapter-fallback off|auto|force`
- `--notes-mode inline|chapter-end|global`
- `--export-manifest off|v1`
- `--quality-report off|v1`
- `--ocr-cleanup off|basic|aggressive`
- `--nav-cleanup off|auto`
- `--filename-scheme index|hash`

## 11) Architecture parity in this repo

Python implementation:

- `epub-utils/parse_epubs.py`
- EPUB parsing via `epub_utils.Document`
- DOM processing via `lxml`

Rust implementation:

- `rbook-utils/src/lib.rs`, `rbook-utils/src/main.rs`
- EPUB parsing via `rbook`
- DOM processing via `kuchiki`

Both follow the same boundary-slicing, media rewrite, CSS collection, and split-output rules.

## 12) Mobile-app implementation blueprint

Recommended module split:

1. `EpubContainer`
   - open zip, parse OPF/manifest/spine/toc
2. `PathResolver`
   - normalize, resolve relative hrefs, detect external src
3. `BoundaryPlanner`
   - convert TOC entries into `(start, end)` chapter windows over spine
4. `DomSlicer`
   - map fragment anchors to top-level body child indexes
5. `Renderer`
   - plain/rich modes + image rewriting hooks
6. `AssetExtractor`
   - dedup extraction of images/styles/media
7. `OutputWriter`
   - single-file or split chapters + stale chapter cleanup
8. `LinkPlanner`
   - internal href rewrite + unresolved target validation
9. `NotesPlanner`
   - `inline`, `chapter-end`, `global` note placement with stable IDs
10. `ReportEmitter`
   - `manifest.v1.json` + `report.v1.json`

### Minimal pseudocode

```text
book = open_epub(path)
meta = read_metadata(book)
manifest, spine, toc = build_indexes(book)
boundaries = toc_to_boundaries(toc, spine) or spine_fallback(spine)

for boundary in boundaries:
  chunks = []
  for spine_doc in docs_between(boundary.start.href, boundary.end.href):
    dom = parse_dom(spine_doc)
    slice = slice_body(dom, boundary.start.fragment?, boundary.end.fragment?)
    css_collector.observe(dom.head)
    chunks += render(slice, mode, image_resolver_for(spine_doc.href))
  section_text = join(chunks)
  emit_section(boundary.label, section_text)

emit_css(mode, style_mode, css_collector)
write_output(single_or_split)
rewrite_internal_links()
emit_manifest_and_report_if_enabled()
```

## 13) Validation checklist for parity

When reproducing this in mobile, validate against this repo outputs:

1. TOC chapter labels and ordering match.
2. Split-title EPUBs include chapter body text (Meditations Books 3-12 case).
3. Images are extracted and links rewritten correctly.
4. Re-running with split mode does not leave stale chapter files.
5. Rich mode preserves tables/figures/classed content.
6. Plain mode excludes `head/title/style/script/svg` leakage.
7. Results remain stable across repeated runs.
8. Heading fallback (`auto`) activates on degenerate TOC books (for example Internet Archive OCR EPUBs) and emits activation warnings.
9. `manifest.v1.json` and `report.v1.json` are deterministic for repeated runs.
10. Split mode with `--filename-scheme hash` keeps file names stable across TOC label churn.

## 14) Known tradeoffs

- Markdown is not a full-fidelity ebook format; rich mode intentionally keeps some HTML.
- CSS fidelity depends on renderer support.
- EPUBs with malformed TOCs may still require heuristics beyond this baseline.

Even with those limits, the boundary-based TOC+spine approach is the strongest foundation we have found for consistent chapter-level extraction quality.
