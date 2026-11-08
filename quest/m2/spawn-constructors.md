# [S] Public constructors spawn nothing, or say why

## Goal

Before the cut publishes them, every public constructor or method in a
published library crate that spawns an async task either returns a handle or
driver its caller polls, or is recorded as kept with its reason. moq-tokio is
the tokio edge and may keep spawning. For example, moq-hls's server spawns in
its own path (`rs/moq-hls/src/server/mod.rs`), and `tokio::spawn` or a
runtime spawn appears outside tests in more than 20 library crates.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs
and remaining quests): ✅ settle the public half before the cut (rejected:
leaving it all to [the remaining library spawns](/quest/m3/spawn-audit.md)).
Changing a public constructor's return type after the cut would break up to
ten published crates at once (moq-hls, moq-rtmp, moq-rtsp, moq-srt,
moq-webrtc, moq-transcode, moq-video, moq-mux, qmux, and others found).

Inventory public spawning entry points with a search per crate, then
convert each to a `kio::Tasks` child or a returned driver, following
[spawn-audit](/quest/m3/spawn-audit.md)'s rule ("poll everywhere, no
spawn"), or list it as kept. spawn-audit keeps the private spawns. Each
converted signature gets a migration note in the PR's Impact section.
