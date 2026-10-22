# [S] Stats report

## Goal

`stats::Registry::report` takes each row's lock once and reads live sessions
without a compare-and-swap. The `stats/session-report` bench registers
transport `Wire`s, so the per-connection wire walk shows up in the measured
drain. The remaining O(live sessions) cost is accepted, and the reason is
recorded in the [epic](/quest/m1/release-perf/README.md).

## Plan

Decided 2026-10-21. `stats/session-report/1000x10` went from 11 us to 87 us
(7.9x, confirmed). The drain runs once a second, so at 10,000 live sessions
this is under 0.01% of a core.

- Cause: [#5731](https://github.com/moq-dev/moq/pull/5731) added per-label
  `sent_bytes`/`received_bytes`, kept by `SessionBytes`/`Arrival`. That keeps
  the write path to one session-local atomic, and the cost moves to the drain:
  `SessionBytes::read` does an `Arc::get_mut` per live session, and the row's
  bytes mutex is taken twice. [#5659](https://github.com/moq-dev/moq/pull/5659)
  (refusals, failures) and [#5773](https://github.com/moq-dev/moq/pull/5773)
  (wire counters, `collect_wire` per report) add smaller fixed costs.
- Labels and per-label bytes stay. Labels are how an embedder buckets
  sessions, such as per project or per signing key. Payload bytes per label
  are what matches payload billing, and wire bytes can't give that. Nothing
  reads them yet. Deleting them was rejected anyway: the cost is negligible,
  and removing them would be a breaking change to the stats wire that would
  have to be redone once a per-key or per-bucket view wants them.
- Cheap fix: in `read()`, use `Arc::strong_count(&arrival.traffic) == 1` plus
  an `Acquire` fence instead of `Arc::get_mut`. This is valid only while no
  `Weak<SessionTraffic>` exists, so check that first. Return
  `live.is_empty()` from the same guard as `read()`, instead of locking a
  second time for the closed check.
- Bench: register a `Wire` per session in `session-report`. `collect_wire`
  walks every live connection (a `Weak` upgrade plus two mutexes) on every
  report and every relay `/metrics` scrape, which walks the registry a second
  time for refusals. Wire counters are billed by embedders, so this walk is
  needed by design. If it turns out significant at realistic scale, apply the
  light fix: keep `WireInner.last` in atomics, and skip the `current` lock
  unless the generation changed. A push design, where writers add straight to
  the label row so the drain costs O(labels), needs its own `/quest-plan`.

No API or wire change.
