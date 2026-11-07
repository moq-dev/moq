# [S] Relay settings parse once into a validated configuration

## Goal

moq-relay turns its CLI, environment and TOML settings into one validated configuration at load, and the rest of the relay takes that type, so a setting cannot be accepted and then ignored. Cross-field rules live in that one step instead of across `Config::resolve`, `Cluster::new`, `Cluster::start` and `Relay::load`. The cases this audit found are refused there:

- Stats enabled on a clustered relay without `stats.node`; or `stats.node` defaults to `cluster.node`. Without either, nodes overwrite each other's `<prefix>/node` broadcast.
- `--cluster-lan` without a QUIC listener; today it advertises SRV port 0.
- `runtime.pin` without `runtime.workers`.

The same parse-once rule applies to the admin endpoints: a repeated key on `/sessions` or `/sessions/revalidate` (`Filter::from_query`) is an error, not last-wins.

Smaller gaps fixed in the same PR:

- Refusal counters (`refusals.rs`) stay bounded with stats disabled, where nothing prunes them.
- `auth.rs` maps every `moq_tokio::Transport` without `unreachable!`.
- `websocket.rs` derives its qmux-01-only ALPN choice from the version instead of a hand-kept list.

## Plan

Found by the 2026-11-07 invariant audit (relay-3, relay-6, relay-7, relay-8, relay-11, relay-12, relay-14).

Decided 2026-11-07 (maintainer, invariant-audit root causes): parse, don't validate. The existing settings drift test stays and covers the new type.
