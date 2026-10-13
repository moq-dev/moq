# [M] Presence wire counters

## Goal

`Presence` gains cumulative `wire_tx` and `wire_rx`, monotonic from relay
boot. On every stats tick the relay adds the delta of each live session's
transport bytes since its last reading. A closing session adds its final
reading. An embedder registers a connection's wire source against a
(tier, root) on its stats handle, and the same tick reads it.

## Plan

- **Sources:**
  - **QUIC and WebTransport:** `bytes_sent` / `bytes_received` from
    `moq_net::session::Stats`. noq maps them to `udp_tx` / `udp_rx`.
  - **WebSocket:** the qmux counters from
    [qmux byte counters](/quest/m1/wire-stats/qmux.md). Until those land, a
    WebSocket session reports 0: an undercount, never a guess.
- **Read the transport directly.** The tick must not re-arm the per-session
  100 ms sampler that moq#5382 stopped for idle sessions. Read the
  connection's counters once per tick, cheaply. Benchmark idle-session CPU
  with many sessions before and after.
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
  stats-feed sum (`Presence::add`) and the delta encoding carry them.
- **Docs:** `doc/concept/stats.md` defines both fields and says
  `Traffic.bytes` is payload.
- **Tests (on controlled time):**
  - a session's wire bytes land under its root and tier;
  - a tier change splits them;
  - a reconnecting source never lowers a counter;
  - a dropped embedder guard adds its last reading.
