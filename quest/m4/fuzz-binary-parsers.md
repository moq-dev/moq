# [S] Fuzz mp4::repair, moq-archive::layout, and moq-loc binary parsers

## Goal

Untrusted binary decoders in `moq-mux` (`container::mp4::repair`),
`moq-archive` (`layout`), and `moq-loc` each have a `cargo-fuzz` target wired
into Nightly's fuzz job and excluded from default workspace builds.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place these three new binary-parser fuzz targets in
`m4` behind the release cut (rejected: folding them into `m1`).

Add libFuzzer targets for the three binary parsers that lacked fuzz coverage in
the audit:

1. `moq_mux::container::mp4::repair` (`rs/moq-mux/src/container/mp4.rs`): fuzz
   arbitrary truncated/corrupted MP4 byte streams through the repair pass and
   subsequent metadata read.
2. `moq_archive::layout` (`rs/moq-archive/src/layout.rs`): fuzz `Object`,
   `End`, and frame/header decoding with arbitrary bytes and validate
   round-trip invariants on accepted inputs.
3. `moq_loc` (`rs/moq-loc/src/lib.rs`): fuzz LOC header extension decoding and
   re-encoding.

Register each target in `.github/workflows/nightly.yml:894` (where the fuzz
matrix runs) and ensure workspace package selection (`sh/rs/select.sh`,
`sh/rs/coverage.sh`) excludes the new `*-fuzz` crates cleanly. Seed each target
with a minimal regression corpus covering the edge cases the pre-cut audit fixed:
`mp4::repair`'s metadata bounds and zero timescales, AV1 LEB128 sizes past
`u32::MAX`, and archive IDs past `ID_MAX` in `Object` and `End`.

Public API: none. Wire: none. Run each new target briefly under `cargo fuzz`
(or replay its corpus) and verify `just check` / `sh/rs/select.test.py`.
