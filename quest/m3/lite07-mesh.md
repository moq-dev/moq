# [XS] The moq.pro mesh runs lite-07

## Goal

Every relay in the moq.pro mesh dials its cluster peers with moq-lite-07, so
each one opts in to hidden broadcasts on the wire.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-09-30 lite-07 ships only as the off-by-default `moq-lite-07-wip`
ALPN, and moq.pro still has its lite-07 rollout quest open
(`quest/m1/lite07.md` in moq-dev/moq.pro). That rollout waits on
[Finalize moq-lite-07](/quest/m1/lite07-finalize.md), so this condition cannot
clear before a release carries the final version.

Decided 2026-10-05: [Cluster routing](/quest/m1/cluster-routing/README.md)'s
route layer lands in lite-07, so the final lite-07 carries ROUTE and
path-less ANNOUNCE instead of hop lists, and loses the `Hop Base`/`Hop Keep`
announce compression. moq.pro's rollout must plan for that wire.
