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
and re-anchor after a declared discontinuity so one `wall` value always
describes one linear mapping over the advertised records. Republish the
rendition's catalog entry once the anchor is set.

Tests: each publisher's catalog carries `wall` after its first frame;
`(wall + pts) / timescale` resolves to the observed duration since the MoQ
epoch; a discontinuity re-anchors rather than stretching the old mapping.

## Related

- [#3021](/quest/m2/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - the moq-gst half, with the reference-timestamp rules
- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - exposes the anchor to browser applications
- [Cross-track correlation](/quest/m3/teleop/correlation.md) - reads the anchor on machines that share a clock
