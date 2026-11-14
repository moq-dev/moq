# [M] Fewer pull cycles while warm routes churn

## Goal

The cache-aware rule's transient pull cycles during churn happen less often
than the 3.3% of churn steps [moq-dev/moq#8732](https://github.com/moq-dev/moq/pull/8732)
measured, or clear faster than its 142 deliveries, without slowing a join the
maintainer has not agreed to slow, and the sweep reports the rate so a change
that raises it shows.

## Plan

Decided 2026-11-14 (maintainer, /quest-complete on #8732): ✅ merge, and plan
a quest to lower the rate (rejected: accept the rate as is, hold the PR,
delete). Planned 2026-11-14 by the
[feature review](/quest/m3/release/feature-review.md), which found the pick
had no quest yet.

What #8732 measured: about 9 in 10 cycle steps leave some reader unfed until
the crossing message lands; #8615's earlier sweep saw 0.39% and 53
deliveries; at seed 3, dropping per-link priorities (2.8 to 3.0%) or letting
carriers exclude the neighbours they serve (2.9 to 3.1%) explained neither the
gap. Start by finding which of the sweep's churn or the per-neighbour offers
causes it, then propose fixes with their join-latency cost.

If [warm routes on the wire](/quest/m2/warm-routes-wire.md) moves the epic
downstream, this moves with it.

Public API: none. Wire: none expected; a rule change that needs one updates
the draft's Warm Routes section.

## Waiting

- Maintainer: whether a fix may add join latency, such as a carrier waiting for a crossing message before it joins (recommended: no; a fix that only clears cycles faster comes first)
