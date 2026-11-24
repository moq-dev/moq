# [XS] The moq.pro mesh runs lite-07

## Goal

Every relay in the moq.pro mesh dials its cluster peers with moq-lite-07, so
each one opts in to hidden broadcasts on the wire.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

moq.pro's lite-07 rollout waits on [Finalize moq-lite-07](/quest/m2/lite07-finalize.md), so this
condition cannot clear before a release carries the final version.

Decided 2026-10-05, and redesigned 2026-11-15 by
[cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md): the final lite-07
carries the route layer of [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987).
Relays route to each other by ROUTE, ROUTE_END, DEAD, and DRAIN under a
Node ID, and every claim belongs to the relay that announces
it, with no hop list on the wire; a claim from a lite-06 peer, or across a
scoped grant, carries a chain of the relays that announced it again. Announce
compression is `Path Base`/`Path Keep`. moq.pro's rollout must plan for that
wire, and for its newest and previous releases interoperating as relays
while a lite-06 relay is still in the mesh.
