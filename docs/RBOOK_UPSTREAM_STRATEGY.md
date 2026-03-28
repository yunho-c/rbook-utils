# Upstream Strategy: `rbook` vs `rbook-utils`

This document defines what should be upstreamed into `rbook` and what should remain in `rbook-utils`.

## Recommendation

Keep `rbook-utils` as a separate, opinionated pipeline layer.

Upstream only reusable, policy-light primitives into `rbook`.

## Why this split works

- `rbook` is a general EPUB engine and should stay broadly reusable.
- `rbook-utils` is product/pipeline logic with strong output opinions.
- Keeping policy-heavy behavior out of `rbook` reduces API churn and maintenance burden.

## Good upstream candidates (`rbook`)

These are generic building blocks likely useful to many downstream tools:

1. Href/path primitives
   - normalization (`./`, `../`, query stripping policy)
   - relative resolution from content href

2. TOC/spine boundary planning primitives
   - flatten TOC entries safely
   - map TOC href/fragment into spine windows

3. Anchor/slicing primitives
   - anchor lookup (`id`, `a[name]`)
   - helper for top-level body child range boundaries

4. Resource resolution conveniences
   - robust resource lookup helpers from manifest + href context
   - low-level media-type classification helpers (where missing)

5. Optional deterministic boundary ID helper
   - stable hash from canonical boundary signature

## Keep in wrapper (`rbook-utils`)

These are product decisions or heuristics that should stay outside core:

1. Markdown rendering policy
   - `plain` vs `rich`
   - HTML-preservation choices

2. CSS output policy
   - inline vs external style emission

3. Output layout policy
   - split chapter naming (`index`/`hash`)
   - file/folder conventions

4. Heuristic cleanup and fallback policy
   - TOC degeneracy thresholds
   - heading-confidence chapter fallback
   - OCR cleanup heuristics
   - nav dedupe behavior

5. Notes/link rewrite policy
   - `--notes-mode` transformations
   - split-aware link rewriting/validation semantics

6. Reporting/manifests for app workflows
   - `manifest.v1.json`
   - `report.v1.json`

## Suggested upstream rollout

1. PR 1: path + href helpers
   - Add API + tests only.

2. PR 2: TOC/spine boundary planner primitives
   - No markdown/output policy.

3. PR 3: anchor/slicing helpers
   - Pure DOM/EPUB primitives with fixtures.

4. Optional PR 4: deterministic boundary ID utility
   - Keep narrow and well-documented.

## API design guardrails for upstream PRs

- Avoid adding output-format assumptions (Markdown, HTML policy, file naming).
- Keep functions composable and side-effect-light.
- Prefer explicit inputs/outputs over global options blobs.
- Include fixture-based regression tests in `rbook`.
- Document behavior around malformed EPUBs and edge cases.

## Decision checklist before upstreaming a feature

Ask:

1. Is this broadly useful across multiple downstream apps?
2. Does it avoid embedding content/output policy?
3. Can it be tested independently of markdown/file-writing behavior?
4. Would adding it to `rbook` reduce duplication without forcing heuristics?

If any answer is "no", keep it in `rbook-utils`.
