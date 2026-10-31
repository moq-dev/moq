# [M] The Rust player and gateways report stats and feedback

## Goal

- `moq play`, given `--echo <name>`, publishes an `.echo` broadcast under each
  watched catalog's echo path and fills its feedback track with what it
  received, decoded, and played.
- The gateway imports (`hls`, `rtmp`, `srt`, `rtsp`, `rtc`) and `archive` take
  `--report` and `--echo-path`, which set the catalog's `stats` and `echo`
  sections as they do for the stdin imports and capture.
- `import capture --report` counts the audio frames a microphone capture
  dropped in `skipped`, as it already does for video.

## Plan

- `import --report` and `--echo-path` already cover the stdin imports and
  capture, and `export --echo` and the `export report` and `export echo` sinks
  already read and print the reports (`rs/moq-mux/src/stats.rs`,
  `rs/moq-cli/src/report.rs`). The catalog's `Sent` registry counts what every
  track minted under it writes, and an encoder reports its skips and target
  through `container::Producer::report_skipped` and `report_bitrate`. This quest
  adds the counters still missing and reuses the rest; `--report` and
  `--echo-path` refuse the gateways until it lands.
- Counters come from the source, per track, as cumulative `stats()`
  snapshots: no callback, no per-frame lock.
  - Viewer: the decode and render paths count received, decoded, late,
    stalled, underruns, and errors. `underruns` in
    `rs/moq-audio/src/playback` is already counted privately. The player reads
    video through `moq_mux::container::Consumer` directly, so it bypasses the
    `Source`'s `Received` registry today.
  - Audio capture: `capture::Samples::gap` only flags that buffers were
    dropped. Counting `skipped` needs the drop size, converted to codec frames.
- The gateways build their catalogs in their own crates (`moq-rtmp`,
  `moq-srt`, `moq-rtc`, `moq-archive`; `hls` and `rtsp` build theirs in
  `rs/moq-cli`), so each needs a way to name the stats track and the echo path
  before its first catalog, and a reporter to run beside each session's
  catalog. The report writer lives in `rs/moq-cli/src/report.rs` and reads the
  transport from a `moq_tokio` connection monitor, which the gateway crates do
  not see.
- Decided 2026-10-31 (maintainer): move the report writer into `moq-mux`, with
  the transport passed in as data rather than read from a `moq_tokio` monitor,
  and give each gateway builder `with_report` and `with_echo`. A library user
  embedding a gateway gets reports too; the cost, accepted, is that `Stats`
  becomes a library API. Rejected: handing each session's catalog back to the
  CLI through a returned handle, which keeps the writer in `moq-cli` but leaves
  library users without reports.
- `moq play --echo` reconciles each watched catalog's `echo` section as
  `export --echo` does, and refuses `--catalog-format msf` the same way: the
  MSF catalog does not carry an `echo` solicitation. `play` attaches only the
  consume direction today, so it also needs the publish direction.
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
