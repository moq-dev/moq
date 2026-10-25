# [M] moq-hls export runs on kio

## Goal

moq-hls export's background work (the catalog, timeline, and spans watchers in
`rs/moq-hls/src/export/{mod,rendition}.rs`, and the init build) runs as
poll-driven `kio` tasks the export owns, not on `tokio::spawn`, as
`rs/AGENTS.md` prefers.

## Plan

Do it crate-wide in one PR. Tasks end with their owner instead of being
detached, which also removes the abort handles kept for them today. Keep the
timeline watcher's two lifetime rules: a new reference timeline replaces the
old watcher, and a recording cursor that outlives the `Broadcaster` keeps the
watcher running until the timeline ends (see `Broadcaster`'s `Drop`). Test
both. Tests
keep their own spawns where they drive a runtime. Measure export throughput
before and after with the existing moq-hls benchmarks.

Public API: none expected. Wire: none.
