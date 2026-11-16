# [M] The catalog's clock and join interval live under a sync section

## Goal

The hang catalog's root section for the broadcast's timeline is named `sync`
and is flat: `sync.clock` is the wall-clock time of PTS zero, and
`sync.interval` is how often a viewer can join every audio and video track.
The `media` section, its nested `join` object, and their Rust and JS types
are gone. `rs/hang`, `js/hang`, the hang draft, the docs, and every reader
and writer in the tree use the new shape.

```json
{ "sync": { "clock": { "wall": 1751846400000000 }, "interval": 2000 } }
```

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ The section is named `sync` (rejected: `clock` back at the root with
  `media` keeping only `join`, the audit's recommendation; keeping `media`
  and redefining it in the draft as the broadcast's timeline; leaving it).
  `media` is defined as "what applies to every audio and video track", and
  the clock applies to more than those.
- ✅ The interval is `sync.interval` (rejected: `sync.join.interval`). The
  nesting only left room for "a later join mechanism that is not a fixed
  grid", which nothing plans.

Facts found by the audit:

- [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652) moved the
  root `clock` under a new `media` section and gave no reason beyond
  grouping it with the new interval.
- The clock times text cues, data tracks, `m2ts` frames, stats `arrival`,
  and the catalog track itself
  (`rs/hang/src/catalog/root.rs`: "snapshots share the broadcast clock with
  media"), which the draft's Catalog section makes normative: an update
  takes effect at its frame's timestamp, citing `{{clock}}`, so retarget
  that reference with the rename.
  The draft's own Clock section calls it "the broadcast's one continuous
  clock".
- hang-03 and `release` carry `clock` at the root, so a published 0.21
  player already loses wall-clock time against main. That stays true: a
  second copy at the root would have to be written forever, so recommend
  writing none and saying so in the Impact section.
- moq.pro's staging build has already ported to `media.clock` and ports
  again.
- The section touches about 30 files: `rs/hang/src/catalog/{media,root,mod}.rs`,
  moq-mux (13 files, `catalog/interval.rs` and the producers), moq-hls (8,
  the grid), moq-transcode, moq-video, moq-audio, moq-cli, moq-gst, and the
  moq-c and moq-ffi tests; `js/hang/src/catalog/{media,root,clock,include}.ts`
  and `js/publish` (5 files); `doc/bin/hls.md`, `doc/concept/{hang,archives}.md`,
  `doc/setup/upgrade.md`, and the interop clients. Find them with
  `grep -rn 'media\.clock\|media\.join\|media\.interval()\|catalog::Join'`.

For the PR:

- Rust: `hang::catalog::Media` becomes `Sync` with `clock` and `interval`
  (`Option<Duration>`, whole milliseconds rounded up, as `Join::interval` is
  today), `Catalog::media` becomes `Catalog::sync`, and `Join`, `JoinWire`,
  and `Media::interval()` go. JS mirrors it: `root.sync`, `SyncSchema`.
- The section reads under the draft Root section's one rule, as every root
  section does (`deserialize_object` for the section and `deserialize_some`
  for its members in Rust, plain `z.optional` in JS): a `sync` that is not
  an object or is `null`, a malformed `clock`, or an `interval` that is not
  a positive integer refuses the catalog. A root `media` key is then an
  application's own again, and is ignored like the retired root `clock`
  (`a_retired_root_clock_and_hls_field_are_ignored`). Move the `media` cases
  in `rs/hang/fixtures/catalog-parity.json` to `sync`.
- Draft: rename the Media section and its anchors, fold Join into it, and
  rewrite the hang-04 changelog lines that describe `media`. The mpegts
  draft and the HLS section cite the interval by name.
- Rename the quests that quote the old names in the same PR:
  `quest/m3/layers/`, `quest/m4/hang-thumbnail.md`, and
  `quest/m2/time-addressing/consumers.md`.
- moq-hls reads the interval for its grid, so fix its stale crate metadata
  while there: `rs/moq-hls/Cargo.toml` (description and the `ll-hls`
  keyword) and `rs/moq-hls/src/lib.rs` advertise LL-HLS, which the crate
  does not implement (no `EXT-X-PART`, `PRELOAD-HINT`, or `_HLS_msn` in
  `rs/moq-hls/src`), and describe "HLS record tracks", which #7652 removed.

Tests: the catalog fixtures and `parity.test.ts` round-trip the new shape in
both languages; a non-object `sync` and a zero `interval` refuse; a catalog
carrying the old `media` section reads with no clock and no interval.

Public API: breaking. Rust `hang::catalog::{Media, Join}` and
`Catalog::media`; JS `root.media`, `MediaSchema`, `JoinSchema`. Wire: the
hang-04 catalog JSON, unpublished; a main-built reader of `media.clock`
finds no clock in a new catalog.

## Related

