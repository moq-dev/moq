# [M] A transport stream carried whole is listed under the mpegts section

## Goal

The hang catalog has one MPEG-TS section. The track that carries a
transport stream whole (today the root `m2ts` section) is listed inside
`mpegts`, beside the description of a demultiplexed stream, and the section
has one `muxRate`. `hang::Catalog` has no MPEG-TS field, so the Rust and JS
core catalogs hold the same sections, and the MPEG-TS draft and the hang
draft agree on where the section is defined.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `m2ts` nests under `mpegts` with one `muxRate` (rejected: keep the
  root key and add it to `js/hang` and the hang draft's root schema; keep
  as is).

Facts found by the audit:

- `m2ts` ([moq-dev/moq#5003](https://github.com/moq-dev/moq/pull/5003)) is
  a typed field of the core catalog (`rs/hang/src/catalog/root.rs`,
  `rs/hang/src/catalog/m2ts.rs`), while its sibling `mpegts` is a moq-mux
  extension section (`Mpegts` and `Ext` in
  `rs/moq-mux/src/container/ts/catalog.rs`). Both are specified in
  `drafts/draft-lcurley-moq-mpegts.md`, neither in the hang draft's root
  schema.
- `js/hang` parses neither (`grep -rn 'm2ts\|mpegts' js/hang/src/catalog`
  is empty), so the two core catalogs disagree about a root member.
- Both sections carry `muxRate` (`M2ts::mux_rate`, `Mpegts::mux_rate`), and
  a track may appear in only one of them.
- The names do not say which is which, and `.m2ts` usually means 192-byte
  Blu-ray packets, which the section refuses.
- Users of `M2ts`: `rs/moq-mux/src/container/ts/{passthrough,release}.rs`,
  `rs/moq-mux/src/catalog/producer.rs`, `rs/moq-mux/src/import/format.rs`,
  and `rs/moq-cli/src/{args,publish,subscribe}.rs`.

Recommendations for the PR:

- Shape: `mpegts.passthrough: { track, randomAccess }`, with the rate in
  the section's existing `muxRate`. Pick the member name in the PR;
  `passthrough` matches `moq import ts --passthrough` and the module.
- `M2ts` leaves `hang` for `moq_mux::container::ts` beside `Mpegts`. Check
  first why `rs/moq-mux/src/catalog/producer.rs` reads it from the base
  catalog and not the extension: that is the one thing that could make the
  move cost more than a rename.
- JS gets no MPEG-TS schema: nothing in JS reads a transport stream, and
  with the root member gone Rust and JS agree on the core catalog. Say so
  where `js/hang`'s root schema lists its sections. (This reads the
  decision's "in Rust, JS, and the draft" as parity of the core catalog; a
  typed `mpegts` schema in `@moq/hang` waits for a JS reader.)
- Drafts: the MPEG-TS draft's `m2ts` section becomes a member of its
  `mpegts` section, with one `muxRate` definition, and its MSFTS mapping
  follows. The hang draft's "MPEG-TS Service Information" section, 51
  lines that repeat the MPEG-TS draft's `si` and SI track sections, is
  replaced by a pointer.
- An older main build that wrote a root `m2ts` is unpublished, so no reader
  keeps the old key.

Tests: the passthrough round trip (`passthrough_test.rs`, `release_test.rs`)
reads the track from the nested member; a catalog with both a demultiplexed
description and a passthrough track is refused if they name the same track.

Public API: breaking. Rust `hang::catalog::M2ts` and `Catalog::m2ts` move to
moq-mux. Wire: the unpublished `m2ts` root section of the catalog.

## Related

- [One rule refuses a mis-shaped catalog section](/quest/m2/hang04/shape.md) - `m2ts` is one of the root keys its rule would otherwise cover
