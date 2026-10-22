# [M] FETCH fan-out

## Goal

When many viewers FETCH the same group through a relay over moq-lite, the
relay fetches it upstream once and serves later viewers from its cache. Cost
per viewer falls as viewers grow, as it does on `release`.

## Plan

Measured 2026-10-21 (`session_delivery_group_cost/.../fetch` on lite-06,
cost per viewer):

| Viewers | release | main nightly |
|---|---|---|
| 1 | 57 us | 67 us |
| 16 | n/a | 61 us |
| 64 | 16.5 us | 59 us |

Main's non-fetch path still amortizes (55 us down to 34 us), so on main every
viewer's FETCH appears to cost as much as the first. The relay seems to go
upstream or rebuild state for each fetch. Lite's `IDLE_LINGER` should keep
the relay's copy and cache, so a simple "the track was dropped" explanation is
not enough.

The cache-bypass hypothesis was checked on 2026-10-22. A deterministic
publisher -> relay -> 64 independent viewers test fetches three groups,
letting every prior viewer finish and leave before the next starts. Lite-05,
lite-06, and lite-07 each send exactly one upstream FETCH per group, one
TRACK metadata request in total, and no SUBSCRIBE. This test now runs in the
merge gate; one-relay lite FETCH benchmark fixtures assert the same cache
sharing during setup, outside the timed rounds.

The cost is in repeated logical-track demand, not N upstream requests. A
held metadata-query control avoids the idle/query/splice transitions while
still using a fresh FETCH for every viewer/group. The
`session_delivery_fetch_demand` benchmark pairs both forms over 1/16
publishers and 1/16/64 viewers, using the same payload, grouping and cache
clock as `session_delivery_group_cost`. It runs in the normal session
benchmark target; `SESSION_CPU=1` selects thread CPU time.

Three paired thread-CPU rounds (15 samples, 0.3 s warmup, 1 s measurement
per case) on 2026-10-22 confirmed the control. Each cell is the median of
the three per-run medians, in us per viewer:

| Publishers | Viewers | Normal FETCH | Held-query control |
|---|---|---|---|
| 1 | 1 | 41.7 | 42.0 |
| 1 | 16 | 35.5 | 21.0 |
| 1 | 64 | 34.5 | 19.9 |
| 16 | 1 | 42.0 | 41.0 |
| 16 | 16 | 64.2 | 45.6 |
| 16 | 64 | 41.4 | 25.3 |

The one-publisher 64-viewer normal runs ranged from 33.7 to 34.7 us per
viewer, versus 19.5 to 20.0 us for the control. These are the existing
production behavior and a diagnostic workload, not before/after results.
Permanently holding queries would change demand ownership and is not a
production fix. The optimization remains unfinished.

Remaining work:

1. Remove accidental per-FETCH demand/setup cost while preserving ownership:
   a fetched group must keep the track it reads alive, idle copies must
   release live subscription/query demand, and a route leaving must not
   leave a stale copy serving indefinitely. Reusing a weak cached pointer
   alone still raises logical Front demand and does not remove this cost.
2. Keep partial, failed, expired, and headless cached groups on the normal
   current-route path. Retain the caller's age cap, frame start, egress
   accounting and upstream refusal behavior on any cache-hit path.
3. Re-measure both benchmark forms across the publisher/viewer sweep. The
   existing `/fetch` workload must amortize without a synthetic held query.
   If part of the remaining cost is accepted for deliberate ownership or
   failover behavior, record that decision in the release-performance epic.

There is no N-upstream-request amplification to raise to m0 from the
measured case. Public API: none so far. Wire: none so far.
