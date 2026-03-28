# EPUB Parser TODO

This file is the execution board for the lockstep roadmap across:
- `epub-utils`
- `rbook-utils`

Legend:
- `[x]` done
- `[ ]` pending
- `~` partial/in progress

## M0: Milestone Skeleton

- `[x]` Converted roadmap into milestone IDs (`M1..M8`)
- `[x]` Added explicit parity requirement for each milestone
- `[x]` Added acceptance criteria per milestone

## M1: Internal Link Rewrite + Validation

[P0]

Status:
- `epub-utils`: `[x]`
- `rbook-utils`: `[x]`
- Tests/parity: `~`

Scope:
- Rewrite internal links for single-file and split-chapter outputs.
- Validate unresolved targets and report broken internal links.
- Keep unknown links unchanged (warn + report).

Acceptance:
- Internal anchor links resolve in split output.
- Cross-chapter links are rewritten to chapter-relative paths in split mode.
- Unresolved links are counted in `report.v1.json`.

## M2: Footnotes/Endnotes Modes

[P0]

Status:
- `epub-utils`: `[x]` (markdown-footnote pattern support)
- `rbook-utils`: `[x]` (markdown-footnote pattern support)
- Tests/parity: `~`

Scope:
- Add `--notes-mode inline|chapter-end|global`.
- `inline`: leave notes unchanged.
- `chapter-end`: collect markdown note definitions at chapter end.
- `global`: collect markdown note definitions into `results/<book_slug>/notes.md`.
- Expand note detection beyond markdown-footnote patterns:
  - EPUB semantic note refs (`epub:type="noteref"`, common note/backlink conventions)
  - endnote sections that are not converted into markdown-footnote syntax

Acceptance:
- No-op with `inline`.
- Chapter-end/global outputs produce stable note IDs and backlinks for markdown footnote refs.
- Semantic note patterns (when present in source XHTML) resolve into the selected notes mode.

## M3: TOC + Landmarks + Page-List Export

[P1]

Status:
- `epub-utils`: `[x]` (`manifest.v1.json`)
- `rbook-utils`: `[x]` (`manifest.v1.json`)
- Tests/parity: `~`

Scope:
- Add `--export-manifest off|v1`.
- Emit `manifest.v1.json` with:
  - `schema_version`, `book`, `spine`, `toc_tree`, `sections`,
  - `landmarks`, `page_list`, `assets`, `build`.
- Include stable `section_id`, source boundary mapping, and output path.
- Add schema contract docs + compatibility policy:
  - versioning rules (`v1` evolution)
  - required/optional fields
  - forward/backward compatibility expectations
- Replace placeholder-empty `landmarks`/`page_list` with parsed values when source EPUB provides them.

Acceptance:
- Manifest exists when enabled and contains deterministic section IDs.

## M4: Navigation Dedupe + OCR Cleanup

[P1]

Status:
- `epub-utils`: `[x]`
- `rbook-utils`: `[x]`
- Tests/parity: `~`

Scope:
- Add `--nav-cleanup off|auto` (default `auto`).
- Add `--ocr-cleanup off|basic|aggressive` (default `off`).
- Dedupe noisy/duplicate TOC entries.
- Apply optional OCR cleanup (dehyphenation + repeated/noise line suppression).

Acceptance:
- TOC duplicate suppression counts are reported.
- OCR cleanup changes are counted in `report.v1.json`.

## M5: Asset Completeness + EPUB3 Media Handling

[P1]

Status:
- `epub-utils`: `~` (images + manifest audio/video extraction)
- `rbook-utils`: `~` (images + manifest audio/video extraction)
- Tests/parity: `~`

Scope:
- Preserve existing image behavior.
- Expand `--media-all` to include optional audio/video extraction.
- Keep external/data URIs unchanged.
- Add richer in-content asset reference handling:
  - `srcset`
  - `<picture><source ...>`
  - SVG-linked image references
  - CSS `url(...)` references where applicable in rich/external style paths

Acceptance:
- Image extraction remains backward compatible.
- Audio/video extracted when discoverable from manifest and `--media-all` is enabled.
- Richer asset refs resolve/extract with path rewriting in both split and single-file modes.

## M6: Typography/Semantics Normalization

[P2]

Status:
- `epub-utils`: `[ ]`
- `rbook-utils`: `[ ]`
- Tests/parity: `[ ]`

Scope:
- Improve normalization for lists/quotes/code/tables/epigraph conventions.
- Preserve safe rich fallback where markdown conversion is lossy.

Acceptance:
- No regressions in existing books while improving semantic fidelity.

## M7: Deterministic IDs + Stable Filename Scheme

[P1]

Status:
- `epub-utils`: `[x]`
- `rbook-utils`: `[x]`
- Tests/parity: `~`

Scope:
- Canonical section boundary hashing -> stable `section_id`.
- Add `--filename-scheme index|hash` (default `index`) for split output.

Acceptance:
- Stable mode produces deterministic chapter filenames independent of label order churn.

## M8: Quality Report + Parity Gate

[P0]

Status:
- `epub-utils`: `[x]` (`report.v1.json`)
- `rbook-utils`: `[x]` (`report.v1.json`)
- Tests/parity gate: `~`

Scope:
- Add `--quality-report off|v1`.
- Emit report including:
  - TOC/fallback/link/asset/OCR/cleanup/notes stats
  - warnings/errors arrays
- Add parity checks across both implementations.
- Add automated parity fixture runner in CI:
  - deterministic fixture corpus
  - golden checks for section counts/titles/IDs
  - unresolved-link and missing-asset thresholds
- Track unresolved-link regressions:
  - baseline known unresolveds per fixture
  - fail CI on unexpected increases

Acceptance:
- Both parsers generate machine-readable report files with matching key metrics.
- Parity runs compare section counts + unresolved link counts + missing asset counts.

## Final Integration Gate

- `[ ]` [P0] Run lockstep commands for both parsers on reference books (`Alice`, `Meditations`, `Algebra`, `CBT`, `Ultralearning`).
- `[ ]` [P0] Compare:
  - section counts
  - first N section titles/IDs
  - unresolved link counts
  - missing asset counts
- `[ ]` [P1] Document known parity differences.
- `[ ]` [P1] Publish schema docs for `manifest.v1.json` and `report.v1.json` as part of release notes.
- `[ ]` [P0] Run fixture-based CI parity job successfully on target platforms.
