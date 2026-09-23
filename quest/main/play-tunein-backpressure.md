# [M] play: a tune-in burst must not stall behind the video queue

## Goal

`moq play --delay` above roughly a second reaches the live edge as quickly as
the default does, without retaining a delay's worth of raw decoded surfaces.
Today a burst fills the 30-frame video queue and parks the decoder before the
playout clock observes the remaining timestamps. The picture can settle a
further second behind live and stay there for the session.

## Plan

The chosen A/V policy is to retain encoded frames across the full `max_age`
window, decode only a few frames ahead of presentation, and evict the oldest
queued frame when that small decoded queue fills. Video must continue observing
the live edge without moving the playout anchor while audio owns it. Preserve
the decoder's newest-group startup, transport age bound, container
restarts/discontinuities, end-of-track flush, and reordered codec output.

The current `moq_video::decode::Consumer` owns both the encoded container reader
and native decoder and returns only raw `Frame`s, so the encoded/decoded boundary
must be separated or composed without duplicating its subscription and codec
rules. Bound the encoded buffer by media age and account for its byte footprint;
the `--delay` limit is 10s, and retaining 300 raw 1080p NV12 frames at 30fps
would cost about 900 MB. A compressed window should avoid that raw-surface cost.

A deterministic red regression is in `play::media::tests` (requires the CLI
`play` feature). It feeds 61 timestamps at 30fps with a 2s delay and no window
drain. The original queue parks at frame 31; the final frame's predicted due
time is 990ms late. The same test should pass once the encoded reader can
advance through the burst while decoded surfaces remain bounded. Also cover
video-only and speaker-owned anchors, delayed window drains, timestamp
reordering, discontinuity, and decoder tail flush. The nightly
`just rs features` lane runs CLI play-feature tests.

## Related

- [Playout clock](https://github.com/moq-dev/moq/pull/3528) - added the clock this bounds
