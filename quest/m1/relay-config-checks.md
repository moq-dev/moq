# [S] Relay settings and admin input that cannot work are refused

## Goal

moq-relay refuses at load or request time what it currently accepts and then ignores or mishandles:

- Stats enabled on a clustered relay without `stats.node` is refused, or `stats.node` defaults to `cluster.node`, so nodes stop overwriting each other's `<prefix>/node` broadcast.
- `runtime.pin` without `runtime.workers` is refused, matching the file's "refused rather than ignored" rule.
- A repeated key on `/sessions` or `/sessions/revalidate` (`session.rs` `Filter::from_query`) is an error instead of the last value winning.
- Refusal counters (`refusals.rs`) stay bounded with stats disabled, where nothing calls the pruning `Registry::report`.
- `auth.rs` maps every `moq_tokio::Transport` without `unreachable!` (the enum is `#[non_exhaustive]`), and the qmux-01-only ALPN choice in `websocket.rs` is derived from the version, not a hand-kept string list.
- `session::Registry` registration drops only their own entry, so an embedder registering a reused `Request` id cannot unlist another live session.

## Plan

Found by the 2026-11-07 invariant audit (relay-3, relay-6, relay-8, relay-11 to relay-14).
