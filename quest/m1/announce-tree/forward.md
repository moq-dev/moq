# [L] Pruned forwarding

## Goal

Authenticated lite-07 cluster sessions suppress only announcements proven
strictly worse and unnecessary under the validated standby policy. On the same
workload, total control cost improves against flooding while preserving the
policy's selection, handoff, and recovery contract. Customers and unsupported
sessions keep today's behavior; there is no unconditional copy-count bound.

## Plan

Apply the [policy](/quest/m1/announce-tree/policy.md) in the shared cursor layer
in rs/moq-net/src/model/origin.rs. Keep normal prefix/scope selection, split
horizon, and source identity semantics. Equal-ranked paths remain eligible.
A selected parent is not evidence that its route is ready, and a backup cannot
be promised from reachability when its cursor actually chooses another source.

Reevaluate affected cursors when either route metadata or suppression evidence
changes. During healthy replacement, establish usable advertisements before
ending old ones. On invalidation, explicitly restore forwarding; a stale table
entry must never keep suppressing discovery. Session closure clears its evidence.
Unknown chains, unsupported source mappings, older Lite and IETF sessions, and
mixed meshes without sufficient evidence flood normally.

Introduce an explicit relay opt-in, off by default for initial rollout. Disabling
it restores advertisements and clears suppression state on existing sessions,
without reconnecting or moving the pin. Negotiating lite-07 alone is insufficient.
Test rollback and resynchronization alongside every named simulator case.

Benchmark against flooding while sweeping source, prefix, and peer counts.
Include initial discovery, topology changes, retained copies, CPU and memory,
encoded control and announcement bytes, and actual connection bytes. In
particular, measure cursor resynchronization and any per-prefix evidence: moving
the same fanout into a new control message is not a saving.

Run the simulator against the shipping control stream and real origins, and
exercise subscriptions through link/node failures with retained source identity.
Report announcement continuity and media recovery separately. An active backup
announcement does not promise gapless playback. Document the supported cases,
remaining flood traffic, measured savings, and any explicitly approved recovery
tradeoff before enabling the optimization outside tests.

## Required

- [Relay reachability](/quest/m1/announce-tree/reachability.md) - inspectable evidence with forwarding unchanged
- [Cluster simulator](/quest/m1/announce-tree/simulator.md) - differential and transition coverage
