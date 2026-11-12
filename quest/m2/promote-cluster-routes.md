# [XS] Promote the routes epic out of cluster routing

## Goal

`quest/m2/cluster-routing/routes/` is its own epic at `quest/m2/cluster-routes/`,
so no epic in this tree holds another. Cluster routing's README names it as a
blocker instead of a child.

## Plan

Decided 2026-11-09 (user, /quest-plan in
[moq-dev/quest#594](https://github.com/moq-dev/quest/pull/594)): epics no
longer nest, since a nested epic means its parent is too large, and every
epic gets a branch its quests land on. ✅ Promote each nested epic to a
sibling in its milestone (rejected: merge its quests into the parent). The
same PR that planned this promoted `quest/m4/kio-channel-contention/` and
`quest/m4/stream-transports/` out of `quest/m4/perf/`.

It waited because `quest mv` refuses to move a quest with an open PR: the
PR's branch is named after the quest's path. Checked 2026-11-12
(/quest-unblock): no open PR's head is under
`quest/m2/cluster-routing/routes/`. Run
`quest mv quest/m2/cluster-routing/routes quest/m2/cluster-routes` before any
quest under the old path is started.
