# [S] Bounded stats aggregate

## Goal

The stats aggregator's memory is bounded by its keys plus recent churn, not
by every node it has ever seen, while merged traffic totals stay monotonic.

Today `aggregate::Merged` keeps a sticky traffic node with `Reader::Ended`
forever (the `STICKY` unannounce branch in `rs/moq-stats/src/aggregate.rs`),
so a long-lived aggregator's `nodes` grows with every relay restart, scale-out,
or renamed node. This is a bug on main regardless of
[stats epochs](/quest/m1/stats-epoch.md), which make it worse by giving every
restart a new name.

## Plan

Decided (2026-10-02): grace-window fold.

- An Ended sticky entry re-arms if its path returns within a grace window, so a
  reconnect with intact counters is not new traffic.
- After the window, its last counters fold into one retired total per key and
  the entry is dropped. The merged total is live nodes plus the retired total.
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

- [Stats epochs](/quest/m1/stats-epoch.md) - every restarted node gets a new name, so retired entries churn faster
- [Binary delta stats](/quest/m2/stats-delta.md) - also sweeps the aggregate over node count
