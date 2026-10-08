# [M] The Rust CLI, players, encoders, and remuxes report stats and feedback

## Goal

- `moq import --stats` and `moq publish --stats` announce a stats track in
  the catalog and fill it with what they send.
- `moq play` and `moq export`, given `--echo <name>`, publish an `.echo`
  broadcast under each watched catalog's echo path and fill its feedback
  track.
- `moq export stats <broadcast>` prints a broadcast's stats snapshots, and
  `moq export echo <prefix>` prints the feedback of every `.echo` broadcast
  under a prefix, as JSON lines.

## Plan

- Counters come from the source, per track, as cumulative `stats()`
  snapshots: no callback, no per-frame lock.
  - Publisher: the `moq-video` and `moq-audio` encode producers count frames,
    bytes, keyframes, and drops. The `moq-mux` importers count the frames and
    bytes they write per track. A remux never encodes, so without them the
    CLI would have nothing to report for `moq import`.
  - Viewer: the decode and render paths count received, decoded, late,
    stalled, underruns, and errors. The `moq-mux` exporters count the frames
    and bytes they receive per track, so `moq export --echo` reports a row
    for each rendition it remuxes. `underruns` in
    `rs/moq-audio/src/playback` is already counted privately.
- `moq-cli` folds the counters into `hang::stats::Snapshot` on the stats
  interval and writes it through `moq_json::snapshot`, with the `.z` sibling.
  A TS import flattens `ts::stats::Snapshot` in as `mpegts`. `transport` comes from
  the connection's `ConnectionStats`.
- Feedback: after reading a catalog with an `echo` section, the player
  resolves the echo path against the broadcast, appends `<name>.echo`, and
  publishes there with the fixed feedback
  track, keyed by the catalog's rendition IDs. Each catalog update
  reconciles it: a removed `echo` section, a changed path, or no longer
  watching unannounces the old broadcast. A name that is not a single path segment is refused at
  parse time.
- `moq export stats` and `moq export echo` are sinks that skip `.hang` media
  discovery, so they route around `catalog_format`.
- `doc/bin/cli.md` documents the flags and sinks.
- The media test publishes with `--stats` and plays with `--echo` against a
  publisher that solicits feedback. It asserts that the publisher's frame
  count matches what was sent and that the viewer's newest arrival advances.
- An encrypted (E2EE) broadcast refuses `--stats` and `--echo`: they would publish rendition
  IDs and per-track counters in plaintext beside it. Recommended in the
  2026-10-08 audit over encrypting them through the E2EE `Generation`.

## Required

- [Schema](/quest/m1/stats/schema.md) - the sections and snapshot types

## Related

- [E2EE](/quest/m1/e2ee/README.md) - protected broadcasts expose no semantic metadata
