# [M] The Rust player, encoders, and gateways report stats and feedback

## Goal

- `moq import capture --report` announces a stats track in the catalog and
  fills it with what its encoders send, `skipped` and `bitrate` included.
- `moq play`, given `--echo <name>`, publishes an `.echo` broadcast under each
  watched catalog's echo path and fills its feedback track with what it
  received, decoded, and played.
- The gateway imports (`hls`, `rtmp`, `srt`, `rtsp`, `rtc`) and `archive` take
  `--report`.
- Capture, the gateways, and `archive` take `--echo-path`, which sets the
  catalog's `echo` section as it does for the stdin imports.

## Plan

- `moq import --report` for the stdin imports, `moq export --echo`, and the
  `export report` and `export echo` sinks already report what the `moq-mux`
  importers write and exporters read (`rs/moq-mux/src/stats.rs`,
  `rs/moq-cli/src/report.rs`), and `import --echo-path` solicits feedback for
  the stdin imports. This quest adds the counters they lack and reuses the
  rest; `import --report` and `--echo-path` refuse capture and the gateways
  until it lands.
- Counters come from the source, per track, as cumulative `stats()`
  snapshots: no callback, no per-frame lock.
  - Publisher: the `moq-video` and `moq-audio` encode producers create their
    tracks directly on the broadcast, so they count frames, bytes, keyframes,
    and drops themselves, and expose the target bitrate.
  - Viewer: the decode and render paths count received, decoded, late,
    stalled, underruns, and errors. `underruns` in
    `rs/moq-audio/src/playback` is already counted privately.
- The gateways build their catalogs in their own crates, so each needs a way
  to name the stats track and the echo path before its first catalog.
  Capture publishes its catalog as its encoders start, so both sections reach
  it before they do.
- `moq play --echo` reconciles each watched catalog's `echo` section as
  `export --echo` does, and refuses `--catalog-format msf` the same way: the
  MSF catalog does not carry an `echo` solicitation.
- The media test captures with `--report` and plays with `--echo` against a
  publisher that solicits feedback. It asserts the encoder's frame count and
  that the player's newest arrival advances.
- Decided 2026-10-08: no E2EE refusal here. The CLI has no encrypted
  broadcast yet, so [Encrypted native CLI](/quest/m2/e2ee/cli.md) refuses `--report` and
  `--echo` when it adds one: they would publish rendition IDs and per-track
  counters in plaintext beside it.

- Decided 2026-10-22 (maintainer): the publisher's solicit option moved here
  from [encoder feedback](/quest/m3/stats-encoder-feedback.md), which keeps
  only the adaptation loop and reuses `--echo-path`.
- `export --echo` counts no drops in `late`: `Received::report`
  (`rs/moq-mux/src/stats.rs`) fills only frames, bytes, and arrival while the
  TS release section holds the drops (from
  [moq-dev/moq#6654](https://github.com/moq-dev/moq/pull/6654)).

## Related

- [Encrypted native CLI](/quest/m2/e2ee/cli.md) - refuses stats and feedback on an encrypted broadcast
- [Encoder feedback](/quest/m3/stats-encoder-feedback.md) - the adaptation loop that reads what `--echo-path` solicits
