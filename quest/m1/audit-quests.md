# [M] Quest tree audit

## Goal

The quest tree agrees with itself and with `main`: done, stale,
conflicting, and misranked quests are fixed.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Run `/quest-audit`. After a release cut shifts the milestones, also check
that the move left no broken links or misplaced quests. Earlier runs: ad hoc
2026-09-30, 10-05, 10-08, 10-10; recurring 2026-10-19 and 10-20.

Checkpoint, 2026-10-20, against `main` at `96d6b5bd4`:

- Read all 426 documents and the 22 questline branches on origin with nine
  parallel auditors, together with the moq.pro tree, since many of its quests
  wait on ours. `quest check` was clean, so every finding was content.
- Settled the four conflicts the 10-19 run left, plus about 20 more with the
  maintainer: e2ee publishes through a moq-net wrapper trait
  ([net-wrap](/quest/m2/net-wrap/README.md)); m1 holds everything before the
  cut, breaking or not; C halves wait on the generated package; the timeline
  is `<track>.hls` with `compress: true`; the publisher priority is the send
  order tiebreak; an auth handle backs one live session; GroupState storage
  is benchmarked before either redesign.
- Retired landed work whose PRs left the quest behind: goaway coverage
  (#5998), the JS dead-route error (#5939), the uring stream header (#5690),
  and the catalog-reader smoke. Deleted 29 merged questline branches.
- Most common findings, so check these first next run: ranks inside a
  milestone README that put a quest above its own prerequisites; "after
  #N" or "held until #N" wording after #N merged; questline branches on
  origin whose directory is gone from `main`; and plans built on a design
  another quest has since replaced (SUBSCRIBE_ALL, the e2ee bridge, the
  per-PR CI model before #6038).
- Unverified, for next run: whether qmux 0.6.4 on crates.io already carries
  the credit fix while web-transport #412 and #413 stay open, which would
  clear the m0 qmux conditions.
