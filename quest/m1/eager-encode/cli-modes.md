# [S] The CLI picks each medium's mode with --video and --audio

## Goal

`moq import capture` takes `--video <on|demand|off>` and
`--audio <on|demand|off>`, default `on`, in place of `--no-video`,
`--no-audio`, and `--on-demand`. `export` and `play` take `--video <on|off>`
and `--audio <on|off>` in place of `--no-video` and `--no-audio`. One
vocabulary across the CLI.

## Plan

Decided 2026-10-29 (maintainer), right after
[moq-dev/moq#7188](https://github.com/moq-dev/moq/pull/7188) shipped
`--on-demand`, which reads vaguely and cannot say which medium it means.

- `on` encodes for the whole publish, `demand` only while subscribed (each
  library's `Capture::on_demand`), and `off` publishes no track of that
  medium. Rejected names: `always`, `never`, `none`, `--video-enabled`,
  `--video-encode`.
- [Raw audio on demand](/quest/m1/eager-encode/raw-audio.md) later adds
  `buffered` to `--audio` and makes it audio's default, so per-medium flags
  avoid a second rename. If raw audio lands first, include `buffered` here.
- In `export` and `play`, `off` keeps today's `--no-*` meaning: select no
  rendition of that medium. Keep the existing refusals, such as an
  elementary-stream export with no video.
- Update `doc/bin/cli.md`, `--help`, the tests, and every in-tree script or
  doc that passes the old flags.

Implementation is checkpointed in [moq-dev/moq#7261](https://github.com/moq-dev/moq/pull/7261).
The source changes and in-tree invocations are updated. Raw-audio buffering
has not landed on `main`, so both capture modes still default to `on` and
`buffered` is not accepted. Remaining work: run the capture/play-enabled CLI
tests and `just ci`, verify the updated examples against the built binary's
`--help`, address findings, then remove this quest. The local attempts were
stopped after `quest gate` kept both builds queued without admitting them;
no Rust build or test result is claimed.

Public API: breaking CLI flags, with an Impact migration note. Wire: none.
