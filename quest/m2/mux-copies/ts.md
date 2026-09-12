# [M] Cut MPEG-TS ingest copies

## Goal

TS import is linear in input size without extra 188-byte copies or
`scratch.drain` shifts. Tracks, PTS, and SCTE-35 match today.

## Plan

SRT `feed` → `scratch.extend_from_slice` → copy 188-byte `pkt` →
`Feed.data.extend_from_slice` → `Read::read` copies into mpeg2ts.
`scratch.drain(..off)` shifts the tail. Then H.264 Split copies NALs
again.

Cursor over scratch (no drain). Feed mpeg2ts from `&pkt` without the mutex
`Feed` bounce, or parse PES without the `Read` adapter. Keep Split changes
in [annexb](/quest/m2/mux-copies/annexb.md).

Acceptance: Criterion `ts.import` on `bbb.ts` / `kyrion_mpeg2av_ac3.ts`;
SRT ingest of the same bytes. Compare CPU vs `ffmpeg -c copy` to null.

## Related

- [#3489](/quest/m2/3489-ts-import-stream-liveness.md) - liveness, not copies
- [Annex-B split](/quest/m2/mux-copies/annexb.md) - NAL copies after PES
