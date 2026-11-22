# [S] Count lite-07 Path Keep by Path::parts

## Goal

Both Rust (`rs/moq-net`) and JS (`js/net`) compute lite-07 `Path Keep` in
decoded path parts per `draft-lcurley-moq-lite.md`, with the temporary
`raw_parts` / `base.split("/")` migration shim removed before lite-07 freezes.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ remove the temporary `raw_parts` migration before
`0.22.0` freezes lite-07, and gate [lite07-finalize](/quest/m2/lite07-finalize.md)
on it (rejected: keeping the temporary migration in lite-07).

- `drafts/draft-lcurley-moq-lite.md:133,494,1344` defines `Path Keep` over
  decoded path parts (where escaped `\/` stays inside a single part).
- `rs/moq-net/src/lite/compress.rs:62-88,247,258,265` carries a `TEMPORARY
  MIGRATION` block (`raw_parts`, `raw_head_len`) that splits on raw `/` bytes,
  and `js/net/src/lite/announce.ts:618-629` mirrors it with `base.split("/")`.
- Delete `raw_parts` and `raw_head_len` in `rs/moq-net/src/lite/compress.rs`,
  computing shared prefix length and head slicing over `Path::parts()`.
- Update `js/net/src/lite/announce.ts:618-629` to compute `Path Keep` over
  decoded path parts rather than raw `base.split("/")`.

Public API: none. Wire: breaking on `moq-lite-07-wip` for paths containing
escaped slashes (`\/`); lands before [lite07-finalize](/quest/m2/lite07-finalize.md)
freezes `moq-lite-07`. Add Rust, JS, and Rust↔JS round-trip tests with paths
containing `\/` and `\\` sharing a prefix.
