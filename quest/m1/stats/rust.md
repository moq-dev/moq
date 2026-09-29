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
- Feedback: for each watched catalog with a `feedback` section, the player
  creates the named track in its `.echo` broadcast and writes
  `hang::feedback::Snapshot` to it. It refuses a name it already serves. A
  path that does not end in `.echo` is refused at parse time.
- `moq export stats` and `moq export echo` are sinks that skip `.hang` media
  discovery, so they route around `catalog_format`.
- `doc/bin/cli.md` documents the flags and sinks.
- The media test publishes with `--stats` and plays with `--echo` against a
  publisher that solicits feedback. It asserts that the publisher's frame
  count matches what was sent and that the viewer's newest arrival advances.

## Required

- [Schema](/quest/m1/stats/schema.md) - the sections and snapshot types
