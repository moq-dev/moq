# m0: release blockers

## Goal

What must land before dev merges into main and the release that follows is
cut: user-visible breakage on a released surface, and the proof that a
regression the merge fixes cannot return. Everything else that is a defect
sits in [m2](/quest/m2/README.md) beside the feature it shares code with.

## Plan

Fix where the defect is; each quest says which branch. Every fix lands with a
regression test per Root Cause First. [Merge dev](/quest/m1/merge-dev.md)
requires this questline, so a quest that stops being a blocker moves to m2
rather than holding the merge.

## Quests

- [Idle consumer](/quest/m0/origin-idle-consumer.md) - moq-net: an origin front drops the source's track when its last reader leaves, so upstream and producer demand end then, not 30 s later

