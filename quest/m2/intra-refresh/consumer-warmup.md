# [M] Transcoder decodes refresh history before each group

## Goal

The transcoder, given a source rendition with `warmup` set, never encodes a
picture its decoder has not fully recovered. Every group it transcodes,
whether fetched on demand for a rung or read live after a skip, is preceded
by enough earlier groups decoded as history that its first picture is clean.
Players stay live with stripes instead, via the browser
[dual decoder](/quest/m2/intra-refresh/dual-decoder.md) and the
[native dual decoder](/quest/m2/intra-refresh/native-dual-decoder.md).
Renditions without `warmup` behave exactly as today.

## Plan

This absorbs the old gradual-recovery quest, which planned to carry
`recovery_frame_cnt` on the wire and count `frame_num`. The catalog duration
replaces both.

Decided in the 2026-10-14 planning interview and its review: the transcoder is
the only consumer that must never show stripes, and it does not use
`decode::Consumer`. Both of its paths assume every group decodes on its own,
which intra refresh breaks:

- `rs/moq-transcode/src/rung.rs` fetches one exact group with `fetch_group`
  and opens a fresh pipeline for it, so the decoder has no reference history.
- `rs/moq-transcode/src/feed.rs` reads the live track in arrival order with
  `recv_group()` and `Container::read`, so it never sees `Read::Gap` and has
  no way to tell a skip from continuity.

The fix is history rather than a withhold rule:

- History: before decoding group N, decode the K preceding groups as
  decode-only input, where K is the fewest groups whose span covers `warmup`.
  With one group per sweep that is one group. The history is fed to the
  decoder and never forwarded to the encoder, so group N's first picture is
  recovered. A group that opens on a true IDR needs no history: check the first
  slice NAL type (H.264 type 5; H.265 `IDR_W_RADL`, `IDR_N_LP`, the BLA types)
  with the NAL walkers in `rs/moq-mux/src/codec/{h264,h265}`. Other codecs
  never set `warmup`.
- Rung fetch: fetch groups N-K through N-1 alongside N, decode them first, and
  start the encoder at N. If history is no longer available, fail the fetch
  loudly instead of encoding stripes.
- Live feed: for a `warmup` rendition, read groups in sequence order, not
  arrival order. Treat a skipped sequence as a discontinuity and decode the K
  groups before the next one as history before forwarding it, the same as a
  cold start. Share the history logic between the feed and rung paths rather
  than writing it twice.
- Regression tests in `rs/moq-transcode` over a synthetic multi-group track
  with `warmup`: a fetched group's first output picture is decoded with its
  history and forwarded clean; a fetch whose history is gone fails; a cold
  feed start and a feed skip both decode history before forwarding; an IDR
  group needs no history; a rendition without `warmup` fetches and reads
  exactly as today.

## Required

- [Plan: recovery boundaries](/quest/m2/intra-refresh/plan-boundary.md) - K derives from `warmup`, which must be a safe upper bound

## Related

- [Native dual decoder](/quest/m2/intra-refresh/native-dual-decoder.md) - the player path, which shows stripes instead
