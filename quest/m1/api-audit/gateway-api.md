# [S] Gateway crate API

## Goal

moq-rtmp and moq-srt expose the entry points their consumers use, under the
same builder names for the same knobs, and moq-rtc has one router path.

## Plan

Decided 2026-10-22 (API audit):

- Delete `moq_rtmp::{run, Config, ActivePaths, PathGuard}` and
  `moq_srt::{run, Config}` (`listen.rs`). moq-cli drives `Server` directly
  and moq.pro uses `Server` and `accept_stream`. This also drops srt
  `Config`'s new `linger`/`stitch` fields that only `run` reached, and the
  two configs' disagreeing names. Fix the Cargo.toml comments that say to
  embed `run`.
- One builder vocabulary: rtmp `Client::with_import_max_age`,
  `with_import_bandwidth`, `with_export_max_delay` and srt `with_max_age`,
  `with_bandwidth`, `with_latency` become `with_max_age(impl
  Into<Option<Duration>>)`, `with_bandwidth`, `with_max_delay`, documented as
  ingest-only or egress-only. `Play::with_max_delay`'s argument stops being
  named `latency`. Drop `moq_rtmp::DEFAULT_MAX_DELAY` (2 s, against the CLI's
  500 ms) and keep one default.
- moq-rtc: `Server::publish_router`/`subscribe_router` forward to
  `whip::router`/`whep::router`; make the latter private. moq.pro uses
  `whip::accept`, which stays.

Public API: moq-rtmp, moq-srt, moq-rtc breaking. Wire: none.
