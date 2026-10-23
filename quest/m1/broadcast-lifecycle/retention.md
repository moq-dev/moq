# [M] Publisher retention in moq-mux

## Goal

moq-cli imports (stdin, HLS), the RTMP, SRT, RTC, and HLS gateways, and
moqsink keep a cleanly ended broadcast announced and serving its finished
tracks for a fixed duration counted from the end, then close it. They no
longer use moq-net's `broadcast::Finish`. The duration is `--linger
<duration>` on moq-cli's publishing subcommands and a `linger` duration
property on moqsink, defaulting to 30s; 0 closes at once. An interrupt
(Ctrl-C, moqsink NULL) ends it at once.

## Plan

Decided with the maintainer on 2026-10-23:

- One small moq-mux helper owns it, shared by moq-cli, `moq-rtmp`,
  `moq-srt`, `moq-rtc`, and moqsink. It finishes the catalog and media
  tracks, holds the broadcast and track producers, and closes the broadcast
  when the deadline passes. moq-net gains nothing.
- The deadline is fixed when the content ends and does not depend on track
  max-age or on when anything is polled. 30s matches today's effective
  behavior (media max-age) and the relay's idle-copy window.
- It ignores a newer instance announced at the path; see the epic.
- `<moq-publish>` and the FFI/C bindings do not use it: they finish their
  catalog and close, and a binding app owns its own process lifetime.
- moq-cli RTSP closes at once today (`rs/moq-cli/src/rtsp.rs`); give it the
  helper too, or say why not.
- Keep each gateway's existing split between a clean end and a failure
  (RTMP unpublish vs disconnect, WHIP DELETE vs a vanished peer); only a
  clean end retains. SRT's split is
  [its own quest](/quest/m1/broadcast-lifecycle/srt-end.md).
- Test the helper with mocked time: a late subscriber gets the whole tail
  before the deadline, and the path is unroutable after it.
- Docs: `doc/bin/cli.md`, `doc/bin/gstreamer.md`, the gateway pages under
  `doc/bin/`.

Public API: moq-mux gains the helper. moq-cli gains `--linger` on publishing
subcommands; `moq export --linger` keeps its meaning until
[TS export follows by cause](/quest/m1/broadcast-lifecycle/ts-follow.md)
renames it. moqsink's `linger` returns as a duration (the boolean never
shipped in a release, so no MIGRATE entry). Wire: none.
