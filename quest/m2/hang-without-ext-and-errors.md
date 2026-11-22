# [XS] Fix hang::Catalog::without_ext and drop dead hang/moq-mux Error variants

## Goal

`hang::Catalog::without_ext` preserves every non-`ext` field (failing to
compile if future fields are omitted), dead `hang::Error` and `moq_mux::Error`
variants are removed, and the unused `url` dependency is dropped from `hang`
and `moq-mux`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ land this breaking cleanup in `m2` before `0.22.0`
freezes `hang` and `moq-mux` (rejected: skipping the API freeze cleanups).

1. **Fix `hang::Catalog::without_ext` (`rs/hang/src/catalog/root.rs:354-362`).**
   - Currently constructs a new `Catalog` using `..Default::default()`, silently
     dropping `json`, `binary`, `stats`, `echo`, `include`, and `skipped`.
   - Destructure `self` (or enumerate every field explicitly without
     `..Default::default()`) so all non-`ext` fields are preserved and any
     newly added `Catalog` field forces a compile error until handled.
2. **Remove dead `Error` variants and unused `url` dependency:**
   - Remove the 6 unconstructed `hang::Error` variants (`Hex`,
     `MissingKeyframe`, `TimestampBackwards`, `Url`, `EmptyGroup`,
     `UnknownFormat`) at `rs/hang/src/error.rs:38,46,50,54,58,62,106-116`.
   - Remove the unconstructed `moq_mux::Error::Url` variant at
     `rs/moq-mux/src/error.rs:128-129,349-354`.
   - Drop the unused `url` dependency from `rs/hang/Cargo.toml:33,47` and
     `rs/moq-mux/Cargo.toml:53`, and regenerate `nix/Cargo.nix` if needed
     (`just rs cargo-nix`).

Public API: breaking in `hang` and `moq-mux` (removes dead `Error` variants;
changes `Catalog::without_ext` behavior to preserve all non-`ext` fields).
Wire: none. Add a unit test populating every `hang::Catalog` field and
asserting `without_ext()` clears only `ext`.
