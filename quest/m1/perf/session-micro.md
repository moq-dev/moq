# [S] Amortize per-datagram routing and per-chunk stats

## Goal

Two small per-packet costs on the session hot path:

- `lite::Subscriber::route_datagram` locks the session-wide
  `Lock<HashMap<u64, TrackEntry>>` and clones the entry for every inbound
  datagram.
- Ingress stats bump the shared per-broadcast-tier counters
  (`Meter::bytes`/`frames`) once per chunk, while egress already amortizes
  them once per prefetch batch.

Make a datagram burst and a chunk burst pay these once, not per packet.

## Plan

- Cache the last-hit route in the datagram receive loop: consecutive
  datagrams overwhelmingly share a subscribe id, so a one-entry cache keyed
  by id (invalidated on unsubscribe, which already owns the map lock) skips
  the map lock and clone on the common path. If the workload shows mixed
  ids, a read-mostly structure is the fallback; measure before reaching for
  it.
- Accumulate ingress counter deltas in the receive loop and flush them to
  the shared `Arc<TierCounters>` line once per poll turn or batch,
  mirroring the egress prefetch shape. Totals must stay exact; only the
  flush cadence changes, and the flush must happen before the loop parks so
  scrapes never miss a settled burst.

Acceptance: CPU on a datagram-heavy bench shape and the video shape via
`just bench BASE` on Linux; stats totals verified unchanged by the existing
counter tests plus one covering the flush-before-park boundary.

## Related

- [Ingest batch](/quest/m1/perf/ingest-batch.md) - the same receive loop's lock and wake cadence
