# [M] Presence wire counters

## Goal

`Presence` gains cumulative `wire_tx` and `wire_rx`. Node totals count since
the stats group's epoch began (a group returning from idle starts a new epoch
at zero). Per-root wire counters start at zero on every presence hold,
including the first, with no retained per-root history while unheld. Each
re-held segment must use a fresh broadcast epoch, track identity, or group
sequence so relay caches cannot replay an earlier segment. On every stats tick the
relay adds the delta of each live session's transport bytes since its last
reading. A closing session adds its final reading. An embedder registers a connection's wire source against a
(tier, root) on its stats handle, and the same tick reads it.

## Plan

- **Sources:**
  - **QUIC and WebTransport:** an optional owned direct counter source across
    transport/session adapters. Native sources are `Send + Sync` (conditional
    bounds on WASM), readable independently of the session handle's lifetime.
    `moq-quic` maps them to `udp_tx` / `udp_rx`. Unsupported/browser transports
    expose no source; no sampler, timer, or poll-request fallback.
  - **WebSocket:** the qmux counters from
    [qmux byte counters](/quest/m2/wire-stats/qmux.md). Until those land, a
    WebSocket session reports 0: an undercount, never a guess.
- **Read the transport directly.** The tick must not re-arm the per-session
  100 ms sampler that moq#5382 stopped for idle sessions. Read the
  connection's counters once per tick, cheaply. The tick's work must scale
  with sources plus held roots, never their product: benchmark idle-relay
  CPU sweeping both the source (session) count and the held per-root
  presence track count, before and after.
- **Ticks, tier changes, and closes:**
  - A session's per-connection counters reset with the connection, so keep
    each session's last reading and add only non-negative deltas.
  - `Session::set_tier` moves later deltas to the new tier.
  - A close takes one final reading. A slightly stale final sample
    under-counts, which is accepted.
- **Embedder API:** a source is a closure returning cumulative
  `(tx, rx)`, registered with a guard. Dropping the guard takes its final
  reading and unregisters it.
- **Serialization:** `#[serde(default)]` on both fields, so an older
  consumer and an older relay interoperate. `Presence` stays `Copy`. The
  stats-feed sum (`Presence::add`), the delta encoding, and the aggregator's
  reset check (`Mergeable::regressed` in `rs/moq-stats/src/aggregate.rs`)
  carry them, so a node whose wire totals drop is a reset.
- **Docs:** `doc/concept/stats.md` defines both fields, says
  `Traffic.bytes` is payload, and says unavailable WebSocket wire counters
  contribute zero until the qmux source is adopted.
- **Tests (on controlled time):**
  - a session's wire bytes land under its root and tier;
  - a tier change splits them;
  - a reconnecting source never lowers a counter;
  - a stats group that idles out and returns starts its new epoch at zero;
  - the aggregator treats a wire-only drop as a reset;
  - a dropped embedder guard adds its last reading.
  - a re-held presence track refreshes idle relay caches before exposing its
    zero snapshot, for both plain and compressed siblings and multiple hops.
