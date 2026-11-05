# [S] Open-GOP MPEG-2 export trims leading B-pictures in zero-stuffed CBR streams

## Goal

A TS export starting or resuming inside an open GOP of MPEG-2 video drops
the orphaned leading B-pictures even when their PES ends on zero stuffing,
as CBR encoders emit, without ever trimming part of a reference picture
whose start code straddles two PES packets. The H.264 and HEVC open-GOP
recovery tests also cover a join at an in-flight group.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7365](https://github.com/moq-dev/moq/pull/7365),
which finished the open-GOP MPEG-2 export quest.

[moq-dev/moq#7365](https://github.com/moq-dev/moq/pull/7365) never trims a PES that ends on a zero byte or inside a picture
header, since the next PES may hold the rest of a reference picture's start
code. Zero-stuffed CBR streams therefore keep some leading B-pictures that
decoders skip. Hold each whole-PES MPEG-2 frame until the next one arrives
(or the track ends), then decide with both in hand: if the next PES does
not continue a start code, the trailing zeros were stuffing and the orphan
can go. At the track's end nothing can prove the start code was complete,
so a held PES is kept untrimmed, as today. The cost is one PES of latency on MPEG-2 video only, which the
export's existing `PesPacketStream::hold` path
(`rs/moq-mux/src/container/ts/import.rs`) and [moq-dev/moq#7365](https://github.com/moq-dev/moq/pull/7365)'s per-track `orphan`
state in `container/ts/export.rs` should absorb; check what the hold does
to the live edge before settling it.

Extend `mpeg2_orphans_are_whole_pes_of_leading_b_pictures` with a
zero-stuffed orphan, a P-picture start code split across PES packets, and
a held final PES ending in an incomplete start code, which stays.
For H.264 (`Open`) and HEVC (`Rasl`) in
`rs/moq-mux/src/container/ts/recovery_test.rs`, add a join with a nonzero
`join` so the export starts at an in-flight group, as the MPEG-2 arm does.

Public API: none. Wire: none.
