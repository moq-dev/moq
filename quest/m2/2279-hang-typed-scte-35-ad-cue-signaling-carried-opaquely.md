# [L] hang: typed SCTE-35 / ad cue signaling (carried opaquely today, unreadable by players)

## Goal

Implement and verify the behavior tracked in [#2279](https://github.com/moq-dev/moq/issues/2279)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: typed signaling plus a player-side cue
event only, prototyped as a CatalogExt. SSAI splits into its own future quest.

### Issue context

Ad insertion: nobody streams commercially without it, and MoQ has no ad story. The pieces to signal a splice already exist but only as opaque MPEG-TS byte carriage, so a cue can ride *through* MoQ but nothing can *act* on one.

MoQ should also be able to do this better than HLS: switching broadcasts is instant, so an ad transition needn't stall or re-buffer the way a discontinuity does.

#### What exists today

**SCTE-35 is carried, byte-faithfully, but never parsed.** It lives entirely in the `moq-mux` MPEG-TS container via the `mpegts` catalog extension:

- `rs/moq-mux/src/container/ts/catalog.rs`  -  `Verbatim { stream_type: u8, framing: Framing, stream_id: Option<u8> }`, `enum Framing { Pes, Section }`. SCTE-35 is `Verbatim::new(0x86, Framing::Section)`. Plus `Descriptor { tag, data }` for the program-level `CUEI` registration.
- **Import**: `rs/moq-mux/src/container/ts/import.rs:404` routes section-framed verbatim PIDs to one MoQ track per ES, each frame a complete `splice_info_section`, byte for byte. Cues are stamped with **video PTS** (`import.rs:2218`). Requires an extension catalog  -  a `Catalog<()>` routes the CUEI PID to `Stream::Ignored`.
- **Export**: `rs/moq-mux/src/container/ts/export.rs:596-613` re-derives the `CUEI` registration; `:834` packetizes private sections. Note section-framed export **requires a video track** for the program clock.
- Real coverage: `ts/import_test.rs:352` (6 real `splice_insert`s from `test_data/scte35/kyrion_dirtystart.ts`, TSDuck-verified), `ts/export_test.rs:565` (TS→MoQ→TS byte-identical round trip), `rs/moq-mux/examples/scte35_inject.rs`. PRs #1617, #1685, #1696.

So: TS in → TS out preserves cues perfectly. But a **browser watching that same broadcast sees nothing**, because the cue is an undecoded private section on a TS-specific track that only the TS exporter understands.

**No ID3, no `emsg`, no generic timed-metadata track type anywhere.** The `scte35` name in `rs/hang/src/catalog/root.rs:363`, `rs/moq-mux/src/catalog/hang/ext.rs:155`, and `rs/moq-json/src/snapshot.rs:667` is **test fixture / doc example only**  -  an illustrative extension, not a shipped schema.

#### What's missing

Three separable things:

1. **A typed, container-independent cue model.** Parse `splice_info_section` into something an application can read (`splice_insert`, `time_signal`, segmentation descriptors, `out_of_network_indicator`, duration, `splice_time` → PTS). A cue should be readable by a browser without knowing MPEG-TS exists.
2. **A catalog section describing where cues live**, so a consumer can find the track. The `mpegts` section is the worked example to copy: `rs/moq-mux/src/container/ts/catalog.rs` defines `struct Ext { mpegts: Mpegts }` + `impl CatalogExt for Ext`, plus a `trait Catalog: CatalogExt` composition hook implemented for `()`/`Extra`/`Ext`.
3. **Something that acts on a cue.** Player-side (CSAI: fire an event, let the app swap sources) and/or server-side (SSAI: splice a different broadcast into the output). Relative broadcast refs (`VideoConfig.broadcast`, `RelativeBroadcastSchema`) already let a catalog point at another broadcast, which is most of the mechanism for a server-side splice.

#### Proposed shape

- A `moq-json::stream` track of typed cue records, mirroring the timeline track's shape (`hang::timeline::Record` on a `moq_json::stream`, one record per frame). Cues are sparse, self-contained, and want lossless ordered delivery  -  `stream` is exactly right, and `RecordExt` is the extension hook.
- Records carry PTS in the same timescale as the timeline section, so cue → group resolution reuses the timeline machinery.
- Prototype **out of tree as a `CatalogExt` first** (exactly like `ts::catalog::Ext`) before touching base `hang`. `rs/hang/src/catalog/root.rs:352` has an `extension_roundtrip` test showing the serde-flatten pattern; JS uses `z.extend(RootSchema, ...)` (`js/hang/src/catalog/root.test.ts:15`) and `RootSchema` is a `z.looseObject` so unknown sections pass through untouched.
- The TS importer keeps its verbatim lane (contribution needs byte fidelity), and **additionally** emits typed cues. The two are complementary, not either/or.

#### Open questions

1. **Typed cues vs. verbatim, or both?** Both, I think: verbatim for TS→TS fidelity, typed for everything else. But then the TS exporter must not double-emit.
2. **Which SCTE-35 subset?** `splice_insert` + `time_signal` + segmentation descriptors covers most real usage. Full SCTE-35 is large. Prefer a maintained crate over hand-rolling if one exists at acceptable quality.
3. **Does this generalize to timed metadata?** ID3, `emsg`, and custom app cues want the same track shape. A generic "timed metadata track" with SCTE-35 as one record type may be the better primitive than an SCTE-35-specific section  -  cf. the Public API Scrutiny rule about composable building blocks over bespoke one-offs.
4. **SSAI is a much bigger scope** (ad decision server, per-viewer manifests, tracking beacons). Suggest this issue covers signaling + a player-side event, and SSAI gets its own.
5. Cue PTS is stamped from **video** PTS on import  -  check that's right for audio-only broadcasts.

#### Branch

`main` if it lands as a `CatalogExt` extension or a new optional section that doesn't break struct literals. Note `hang::catalog::Catalog` (`rs/hang/src/catalog/root.rs:16`) is **not `#[non_exhaustive]`** and has pub `video`/`audio` fields, so adding a field to base `hang` breaks struct-literal construction → that variant is `dev`.

#### Cross-package sync

`rs/hang` ↔ `js/hang`; `rs/moq-mux`; `doc/concept`. If a catalog section lands, `drafts/draft-lcurley-moq-hang.md` in the same PR.

## Closes

- [#2279](https://github.com/moq-dev/moq/issues/2279) - close this issue when the quest finishes
