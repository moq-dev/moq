# [M] Pruning policy

## Goal

A tested policy identifies announcements that cannot improve the receiver's
route selection and are not required by its standby policy. Its evidence,
invalidation, and recovery rules are explicit before the cluster wire is
specified. The existing route order and equal-ranked alternatives remain.

## Plan

Prototype against the [simulator](/quest/m1/announce-tree/simulator.md).
Prefer sharing reachability per source relay to exchanging state for every
broadcast, but establish when that information is sufficient. If a class of
routes cannot be proven safe, keep flooding that class and measure how much
traffic remains eligible. Stop or narrow the optimization if its control and
state costs outweigh the measured savings.

Compare routes in the receiver's context: prefix specificity and authorization
scope, anonymity, publisher identity, local-route preference, warm/cold cost,
hop count, split horizon, drains, and saturation all matter. Do not assume
cost accumulation always preserves strict ordering. Preserve hash and recency
ties rather than changing route_order globally to make pruning work.

Define separately the publisher identity used to resume an existing front and
the relay whose reachability can be shared. An empty local chain belongs to the
local relay before the wire appends its hop. Any unknown hop or unsupported
source mapping disables pruning; scanning past an unknown hop is not evidence.

The policy must answer:

- What proves the receiver has a usable better route, including before the
  first announcement of a new prefix? A requested parent is not proof that
  the route has arrived. Per-source evidence must account for a neighbor
  selecting a different source for the same prefix.
- Which worse routes remain as standbys, and how are suppressed alternatives
  rediscovered on withdrawal, failure, repricing, or restart? Document recovery
  relative to flooding, and obtain a maintainer decision for any tradeoff.
- What proves a retained backup avoids a particular link or node for the
  actual prefix? Prefix-dependent path selection and competing sources must
  not turn a reachability backup into a false protection claim.
- How does replacement readiness cross independent sessions and streams?
  Establish the new route before pruning the old one during healthy changes.
  Define completion and invalidation of snapshots or acknowledgements if used;
  elapsed time is not proof of readiness.
- How do sessions sharing a hop differ? Retaining a second session can protect
  a link only if its identity and lifetime are represented independently.

Publish the chosen state machine and its proof assumptions with the prototype
results. Include message/state costs across relay count, peer count, and prefix
count. Keep the protocol shape open until these cases pass; update the later
quests to match the validated design before implementing them.

## Required

- [Cluster simulator](/quest/m1/announce-tree/simulator.md) - the flooding oracle and reproducible counterexamples
