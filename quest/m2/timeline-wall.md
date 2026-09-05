# [S] Publishers anchor the timeline to wall time

## Goal

Every built-in publisher sets `Timeline.wall` so a consumer can read when
presentation timestamp zero happened on the publisher's clock: the moq-video
capture publisher, the moq-cli import paths, and `js/publish`. moq-gst has its
own quest for the same anchor.

The anchor is data, not a synchronization source. Nothing in the library
times playback against it: frame timestamps stay relative, and two machines'
wall clocks are only comparable when the application already knows they are
synced. Metrics and applications that control both ends may inspect it.

## Plan

`moq_mux::timeline::Producer::set_wall(pts, SystemTime)` and the JS
`setWall(pts, Date)` exist and nothing outside tests calls them. Anchor on the
first frame each publisher emits, with the wall time it observed that frame,
and republish the rendition's catalog entry once the anchor is set.

One `wall` value describes one linear mapping, so a declared discontinuity
that breaks linearity (a seek, a source restart) must not overwrite it:
records already advertised on the timeline track would then resolve against
the wrong epoch in DVR, HLS `PROGRAM-DATE-TIME`, and correlation consumers.
Instead the first timeline record after a discontinuity carries the new
anchor itself, and a consumer resolves a record against the latest anchor at
or before it. The catalog `wall` stays the anchor for records before the
first such carried one. That is a record-schema addition, so it lands with a
`drafts/draft-lcurley-moq-hang.md` update in the same PR.

Tests: each publisher's catalog carries `wall` after its first frame;
`(wall + pts) / timescale` resolves to the observed duration since the MoQ
epoch; after a discontinuity, records before it still resolve against the old
anchor and records after it against the one they carry.

## Related

- [#3021](/quest/m2/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - the moq-gst half, with the reference-timestamp rules
- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - exposes the anchor to browser applications
- [Cross-track correlation](/quest/m3/teleop/correlation.md) - reads the anchor on machines that share a clock
