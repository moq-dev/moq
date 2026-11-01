# [M] The remaining library spawns

## Goal

Every async task spawn left in a library crate after
[Libraries spawn nothing](/quest/m1/poll-first/no-spawn.md) is either converted
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

Public API: constructors that spawned return a handle to poll instead; an Impact
migration note per published crate. Wire: none.

## Required

- [Libraries spawn nothing](/quest/m1/poll-first/no-spawn.md) - the rule and the pattern this applies
