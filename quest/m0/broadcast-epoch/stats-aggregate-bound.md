# [S] Bounded stats aggregate

## Goal

The stats aggregator's memory is bounded by its keys plus recent churn, not
by every node it has ever seen, while merged traffic totals stay monotonic.

Today `aggregate::Merged` keeps a sticky traffic node with `Reader::Ended`
forever (the `STICKY` unannounce branch in `rs/moq-stats/src/aggregate.rs`).
A returning path reuses its entry, but every distinct path ever announced (a
scale-out, a renamed node) stays in a long-lived aggregator's `nodes`. This is
a bug on main regardless of [stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md), which
make it worse by giving every restart a distinct path.

## Plan

Decided (2026-10-02): grace-window fold.

- An Ended sticky entry re-arms if its path returns within a grace window, so a
  reconnect with intact counters is not new traffic.
- After the window, its last counters fold into one retired total per key and
  the entry is dropped. The merged total is live nodes plus the retired total.
- Open: a path that returns after the window with intact counters would count
  its pre-departure traffic twice. Decide whether that bounded error is
  acceptable (rare once the window exceeds reconnect times, and with epochs
  only a same-instance return after a long outage), or whether some bounded
  per-path baseline reconciles it. Test exact totals across that return either
  way, not just monotonicity.
- With an epoch per group announcement, a stats group returning from idle is
  always a new path, so the grace re-arm covers only reconnects of a still
  announced path, and the double-count above needs a same-epoch return.
- Rejected: folding on depart double-counts a node that reconnects with its
  counters intact. TTL eviction without a fold makes merged totals regress.
- Timers follow the repo rule: time decisions use `max(wall, pts)`. Tests mock
  time.
- A regression test churns N distinct nodes and asserts `nodes` stays bounded
  while the merged total never regresses.
- Add or extend a benchmark swept over node count, per the fan-out guidance in
  `AGENTS.md`.

Public API: none expected beyond a possible grace-window knob on
`aggregate::Config`. Wire: none.

## Related

- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - every restarted node gets a new name, so retired entries churn faster
- [Binary delta stats](/quest/m2/stats-delta.md) - also sweeps the aggregate over node count
- [Stats linger](/quest/m0/stats-linger.md) - a group's node broadcast outlives a short gap instead of churning an aggregate entry
