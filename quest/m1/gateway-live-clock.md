# [S] Gateways publish on the broadcast clock

## Goal

`moq-srt`, `moq-rtmp`, and HLS import map source timestamps onto the
catalog's broadcast clock, as `moq import` does since
[#4122](https://github.com/moq-dev/moq/pull/4122). Today only the CLI calls
`.live()`; the gateways publish the encoder's timestamps verbatim, so the
advertised wall time is wrong for a source whose PTS does not start near zero,
and an encoder reconnect that restarts its timestamps rewinds or is refused.

## Plan

Decided: each gateway opts into `live()`. The importer default stays
verbatim, since tests and callers that pin `Config::with_clock` rely on it.

Guidance:

- SRT: `rs/moq-srt/src/ts.rs` builds the `ts::Import`. RTMP: the server's
  publish path and the pull path in `dial.rs` build `FlvImport`.
- HLS import (`rs/moq-hls/src/import.rs`) runs one fMP4 importer per
  rendition, and `live()` gives each its own anchor, so renditions would map
  their first frames to different instants. They share one source clock and
  need one mapping: share the `Anchor` across the broadcast's importers, or
  pick another way to anchor the playlist once. Audio and video in separate
  HLS renditions are the case to test.
- A reconnect that reuses the broadcast is where this pays off; check each
  gateway's reconnect keeps the same importer (and anchor) or deliberately
  starts a new one.
- Tests per gateway: a source starting at a large PTS publishes near the
  broadcast clock's now, and a restart to zero continues forward.
- Docs: the gateway pages under `doc/bin/` that describe timestamps.
