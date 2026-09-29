# [M] The Rust CLI, players, encoders, and remuxes report stats and feedback

## Goal

- `moq import --stats` and `moq publish --stats` announce a stats track in
  the catalog and fill it with what they send.
- `moq play` and `moq export`, given `--echo <path>`, publish an `.echo`
  broadcast and serve a feedback track to every watched broadcast that
  solicits one.
- `moq export stats <broadcast>` prints a broadcast's stats snapshots, and
  `moq export echo <path>` prints an `.echo` broadcast's feedback, as JSON
  lines.

## Plan

- Counters come from the source, per track, as cumulative `stats()`
  snapshots: no callback, no per-frame lock.
  - Publisher: the `moq-video` and `moq-audio` encode producers count frames,
    bytes, keyframes, and drops. The `moq-mux` importers and exporters count
    the frames and bytes they write per track. A remux never encodes, so
    without them the CLI would have nothing to report for `moq import`.
  - Viewer: the decode and render paths count received, decoded, late,
    stalled, underruns, and errors. `underruns` in
    `rs/moq-audio/src/playback` is already counted privately.
- `moq-cli` folds the counters into `hang::stats::Snapshot` on the stats
  interval and writes it through `moq_json::snapshot`, with the `.z` sibling.
  A TS import flattens `ts::Stats` in as `mpegts`. `transport` comes from
  the connection's `ConnectionStats`.
- Feedback: the `.echo` broadcast serves through `broadcast.dynamic()`. Each
  `requested_track()` is accepted, capped for unclaimed names (an unclaimed
  track is dropped once unsubscribed), and answered
  with an empty `hang::echo::Snapshot`; a watched catalog whose `echo`
  section names the track claims it, and the player fills it from then on. A
  second catalog claiming a bound name is refused. A
  path that does not end in `.echo` is refused at parse time.
- `moq export stats` and `moq export echo` are sinks that skip `.hang` media
  discovery, so they route around `catalog_format`.
- `doc/bin/cli.md` documents the flags and sinks.
- The media test publishes with `--stats` and plays with `--echo` against a
  publisher that solicits feedback. A second arm starts the publisher's
  subscription before the viewer reads the catalog, the race this serving
  rule exists for. It asserts that the publisher's frame
  count matches what was sent and that the viewer's newest arrival advances.
- A unit test exceeds the unclaimed cap with sequential requests that each
  disconnect, and a later request is still accepted.

## Required

- [Schema](/quest/m1/stats/schema.md) - the sections and snapshot types
