# [M] Reconcile KeepAlive APIs, send PING-only keep-alive probes, and deduplicate JS QMux pings

## Goal

Before `0.22.0` freezes public APIs: `moq-quic`, `moq-tokio`, `moq-net`, and `qmux` no longer expose three colliding `keep_alive`/`KeepAlive` shapes, Rust and JS agree on initial stale evaluation when `set_keep_alive` is configured, QUIC keep-alive probing sends only bare PING packets (never full-MTU STREAM data past cwnd/pacer) with a small hard cap that re-anchors when `heard` extends `Timer::Silent`, and JS `@moq/qmux` runs a single ping timer instead of parallel `#keepAliveLoop` and `#idleLoop` loops.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ reconcile the `KeepAlive` public API naming/initial-stale semantics and fix both transport bugs before `0.22.0` (rejected: leaving `KeepAlive` naming as-is; deferring past `0.22.0`).

Caused by [moq-dev/moq#9698](https://github.com/moq-dev/moq/pull/9698) (which accepted a probing bug under `afk`).

1. **Public API reconciliation across Rust and JS:**
   - Reconcile the three colliding `keep_alive` / `KeepAlive` shapes: `moq_tokio::quic::Config::keep_alive: Option<cli::Duration>` (`rs/moq-tokio/src/quic.rs:234`), `qmux::ws::KeepAlive { interval, timeout }` (`rs/qmux/src/ws.rs:47-55`), and `moq_quic::KeepAlive { soft, hard }` / `moq_net::transport::KeepAlive { soft, hard }` / JS `Connection.KeepAlive { soft, hard }`. Rename or unify (e.g. `ping_interval` for single-interval NAT keep-alive or unifying on `{ soft, hard }` where appropriate) so public names carry one consistent meaning.
   - Align initial stale evaluation: Rust `Connection::set_keep_alive` (`rs/moq-quic/src/connection/mod.rs:2010`) currently leaves `stale = false` when `now - heard >= soft` until `handle_timeout` fires, whereas JS `#keepAliveLoop` (`js/qmux/src/session.ts:1291`) flips `stale` synchronously. Make Rust evaluate `now - heard >= soft` on `set_keep_alive` too.
2. **PING-only congestion-exempt keep-alive probes (`rs/moq-quic`):**
   - Stop setting `self.spaces[space].loss_probes = 1` in `handle_timeout` (`rs/moq-quic/src/connection/mod.rs:900,2845-2855`), which bypasses cwnd and pacer for full `min_mtu` (~1200 B) STREAM packets (allowing `hard / (soft / 2)`, e.g. 600 packets / ~720 KB, to bypass congestion control via `rs/moq-quic/src/connection/keep_alive.rs:91-94`).
   - Add a dedicated PING-only probe flag so a congestion-exempt keep-alive probe carries only PING (never STREAM data), bound it with a small hard cap per silence episode, and re-anchor `exempt_probes` when a late packet advances `heard` and extends `Timer::Silent` (fixing Sol's AFK-accepted round-3 finding).
3. **Single ping timer in JS `@moq/qmux`:**
   - In `js/qmux/src/session.ts` (`#keepAliveLoop` at `:1291`, `#idleLoop` at `:1366`), suppress `#idleLoop`'s independent `QX_PING` every `idleTimeout / 3` (`:1383-1391`) while `#keepAliveLoop` is active, using one ping timestamp and one timer owner (`REVIEW.md` State).

Tests: `moq-quic` unit tests verifying (a) a congestion-exempt keep-alive probe sends PING only and zero STREAM bytes even when STREAM data is queued and cwnd is full, (b) extending `Timer::Silent` via a late packet continues probing through the new deadline, and (c) `set_keep_alive` after `soft` has elapsed flips `stale` immediately; plus a `@moq/qmux` test verifying no duplicate `QX_PING` frames fire from `#idleLoop` while `keepAlive` is active.

Public API: breaking pre-cut `KeepAlive` naming reconciliation across `moq-tokio`/`qmux`/`moq-quic`/`moq-net` (PR title uses `!` and `## Impact` carries the migration note). Wire: none.

## Related

- [Demote routes on stale sessions and expose draining](/quest/m1/stale-route-demotion.md) - consumes this keep-alive and stale API
