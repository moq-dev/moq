# [M] The remaining library spawns

## Goal

Every async task spawn left in a library crate after
[Libraries spawn nothing](/quest/m1/poll-first/no-spawn/README.md) is either converted
to a `kio::Tasks` child its owner polls or listed with the reason it must stay.
Blocking OS threads are out of scope.

## Plan

Planned 2026-10-25 with the maintainer as the follow-up to no-spawn. As of
`main` today the spawning library crates include moq-tokio (connection,
server, client), moq-relay (web, shutdown, uring), moq-mux's container
consumer, moq-hls's server (its path follower and each
broadcaster's driver), moq-rtmp, moq-rtsp, moq-srt, moq-rtc, moq-transcode,
moq-video's encode producer, moq-cli's play path, moq-bench, and qmux's
transport. Recount with `rg 'tokio::spawn|spawn_local|task::spawn'` outside
tests, benches, and binaries before starting.

Convert where an owner already polls the work; report the rest. Split per
crate group if the PR gets large, keeping one problem per PR.

Added 2026-10-31 from [moq-dev/moq#7324](https://github.com/moq-dev/moq/pull/7324),
which moved moq-hls export's background watchers onto a kio `Driver` that
`Server` and `moq hls --record` spawn at their tokio edge, one task per
broadcaster (`rs/moq-hls/src/server/mod.rs:285` on be39188000; the moq-hls
server entry above):

- The watcher bodies still use `tokio::select!` and
  `tokio::sync::{watch, Mutex}` (`rs/moq-hls/src/export/mod.rs` and
  `rendition.rs`); convert them to `#[kio::machine]` or poll functions.
- [moq-dev/moq#7329](https://github.com/moq-dev/moq/pull/7329) (merged)
  routed the export tests' sleeps and timeouts through `kio::clock`, but 41
  of them (`rs/moq-hls/src/export/`, plain `#[tokio::test]`) still run on
  real time. Move them to the `kio::clock` sim, or paused tokio where the
  sim cannot run them yet.

Public API: constructors that spawned return a handle to poll instead; an Impact
migration note per published crate. Wire: none.

## Required

- [Libraries spawn nothing](/quest/m1/poll-first/no-spawn/README.md) - the rule and the pattern this applies
