# [S] Fix Prefixes UTF-8 slice panic and non-canonical path handling

## Goal

`Prefixes::matches`, `covering`, and `overlaps` in `moq-pattern` never panic on
multi-byte UTF-8 paths and behave consistently on non-canonical paths such as
`"/foo"` or `"/é"`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix high-severity non-breaking bugs in `m1`
(rejected: `m0` with `release` backports; `m3`).

- `rs/moq-pattern/src/prefix.rs:295-315` slices string paths by a raw byte
  index computed from prefix length without validating UTF-8 char boundaries or
  canonical path boundaries. Passing a non-canonical path like `"/é"` panics at
  a non-UTF-8 char boundary, and paths with leading/duplicate slashes like
  `"/foo"` mis-slice.
- Normalize or compare over path parts / safe `strip_prefix` + boundary checks
  (`is_char_boundary` / `Path` semantics) in `Prefixes::matches`, `covering`,
  and `overlaps` so arbitrary `Path` or `&str` inputs can never panic or
  mis-match.

Public API: none. Wire: none. Add unit and property/fuzz regression tests for
`"/é"`, multi-byte UTF-8 prefixes, leading/trailing/duplicate slashes, and
escaped slash segments.
