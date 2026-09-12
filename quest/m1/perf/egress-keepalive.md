# [S] Stamp egress keep-alive once per prefetch fill

## Goal

Fanout send stamps cache expiry once per Prefetch/Buffer fill (or per
`refresh_interval`), not once per batched frame. Group expiry tests still
pass.

## Plan

Prefetch fill already stamps once per 8 frames. `refresh_if_stale` only
re-stamps after `refresh_interval`. The wire publishers then call
`group.keep_alive()` per batched frame (`lite/publisher.rs`,
`ietf/publisher.rs`), which always does `state.read().charge.refresh()` →
`pool.stamp()` → `Instant::now()`. That undoes both amortizations on the
fanout send path.

Reuse `refresh_if_stale` (or stamp once per `poll_read_frames` fill) on
both serve loops. Keep `slow_prefetch_reader_survives_expiry`.

This is model egress stamping, not ingest-batch (ingest lock/clock) and
not uring #3122 (drive loop clock).

Acceptance: existing `group_read_frames` / `track_fanout_group` plus a
lite session drain. Clock reads / `charge.refresh` per delivered frame drop
to ~1 per fill. No expiry regression.

## Related

- [Ingest batch](/quest/m1/perf/ingest-batch.md) - ingest, not egress
- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - uring drive loop
- [kio wake](/quest/m1/perf/kio-wake.md) - park/wake locks, not cache stamps
