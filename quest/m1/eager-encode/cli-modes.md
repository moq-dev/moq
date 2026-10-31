# [S] The CLI picks each medium's mode with --video and --audio

## Goal

`moq import capture` takes `--video <on|demand|off>` and
`--audio <on|buffered|off>`, defaulting to `on` for video and `buffered`
for audio, in place of `--no-video`,
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
- Raw audio on demand landed first
  ([moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238)): audio's
  `Capture::on_demand` now keeps raw PCM while unsubscribed, and capture
  defaults audio to it, with `--on-demand=false` as the interim opt-out.
  Include `buffered` in `--audio` as audio's default. Audio no longer has an
  unbuffered demand mode, so `--audio demand` would mean the same thing.
- In `export` and `play`, `off` keeps today's `--no-*` meaning: select no
  rendition of that medium. Keep the existing refusals, such as an
  elementary-stream export with no video.
- Update `doc/bin/cli.md`, `--help`, the tests, and every in-tree script or
  doc that passes the old flags.

Public API: breaking CLI flags, with an Impact migration note. Wire: none.
