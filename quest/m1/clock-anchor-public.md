# [S] moq-mux exports the per-source clock anchor

## Goal

An application running its own demuxer puts several tracks of one source on
the broadcast `Clock` with one shared offset, as the built-in TS, fMP4, and
FLV importers do, without going through a built-in importer.

## Plan

Requested by an external consumer (OneTooMany, Discord): their MPEG-TS
ingest publishes video, KLV, and sometimes audio from one program. One
`SourceMap` per track lets tracks drift apart by their first-PTS difference,
and one shared `SourceMap` reads interleave (beyond `MAX_REORDER`) as a reset.

Decided: make `clock::Anchor` and `clock::Lane` public with the API the
importers already use: `Anchor::new(clock)`, `translate(&mut lane, pts)`,
`translate_at`, `extend`, and `Lane::restart`, with `Lane: Default`. Additive,
so it lands on `main` for a 0.10 patch.

Open for the PR: the export path. Proposal: `pub mod clock` with
`moq_mux::clock::{Anchor, Lane}`, keeping the root `Clock` re-export. Ask the
maintainer with alternatives.

Document both in `doc/lib/rs/moq-mux.md`, with a two-track example.

Their other `ts::Import` gaps (MPEG-2 video, KLV reassembly) stay unplanned
until requested directly. The H.265 per-packet error is
[TS damaged units](/quest/m1/ts-damaged-units.md).
