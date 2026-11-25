# [S] Reconcile hang-04 catalog validation and codec parsing edge cases

## Goal

`hang::Catalog` parsing enforces rendition validation consistently across entry
points, VP9 codec string parsing rejects invalid `full_range` values, and PCM
`description` validation matches `draft-lcurley-moq-hang.md`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place softer `hang-04` spec/validation edge cases in
`m3` while keeping breaking and higher-impact bugs in `m1`/`m2` (rejected: all
medium bugs in `m1`; all medium bugs in `m3`).

1. **Run `check_renditions()` on `Catalog::from_str` / `Catalog::from_slice`.**
   - `rs/hang/src/catalog/root.rs:394-427` parses catalogs without invoking
     `check_renditions()`, allowing duplicate or conflicting rendition metadata
     to slip through deserialization when callers construct catalogs from wire
     bytes.
   - Call `check_renditions()` in `from_slice` (and verify `from_str` delegates
     to it), checking existing catalog fixtures for any intentional violations.
2. **Restrict `VP9::from_str` `full_range` to `0 | 1`.**
   - `rs/hang/src/catalog/video/vp9.rs:94` parses `full_range` too loosely
     instead of accepting only `0` or `1` per the VP9 codec string grammar.
3. **Reconcile PCM `description` validation with the draft.**
   - Compare the PCM section of `drafts/draft-lcurley-moq-hang.md` (`{#audio-pcm}`) with
     `rs/hang/src/catalog/audio/mod.rs:270-282` (and JS `js/hang` catalog
     validation) so Rust, JS, and the hang draft agree on whether `description`
     is forbidden, optional, or required for PCM renditions.

Public API: none (stricter validation of invalid inputs). Wire: clarifies
`hang-04` edge-case conformance. Add unit tests for each validation rule in
`rs/hang` (and `js/hang` if PCM rules touch both) and run `just drafts check`.
