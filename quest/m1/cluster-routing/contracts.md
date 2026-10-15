# [M] Prove shared routing contracts

## Goal

A reviewed design and deterministic counterexamples settle identity,
forwarding progress, and export policy for the per-origin route layer before
its wire is implemented. Preserve the maintainer's connection priorities and
federation/P2P requirements without claiming guarantees Babel does not supply.
This quest produces fixtures and updated implementation plans, not speculative
production code.

## Plan

Decided 2026-10-13: a process/tab gets a fresh routing identity on restart,
shared by its authenticated live connections. Administrative identity and
broadcast epoch are separate. Work through scoped relay admission, a publisher
reached through CDN and P2P, reconnect without restart, and delayed state from
an earlier incarnation. A random boot ID distinguishes incarnations but does
not alone prove old state cannot return after collection. Resolve how existing
`cluster.id` configuration maps to the new distinction and report the API impact.

Applications assign local per-connection priority, ahead of additive cost.
At equal priority, total cost includes prefix seed and route metric; preserve
deterministic path hashing and newest-first only for otherwise identical
routes. Default unconfigured links reproduce hop counting. Metrics used for
feasibility must strictly increase; settle zero-cost representation and overflow
without silently changing configured price semantics.

Test rather than assume forwarding progress: every hop currently reselects a
path's origin, while Babel guarantees loop freedom per origin. Include a ring
whose nodes prefer different origins of the same prefix/epoch, overlapping
wildcards, and alternate requests that return through the requester. Compare
safe policy constraints or a forwarding constraint that preserves aggregation.
Bring consequential wire/API or policy restrictions back through quest-plan;
do not discard application priorities or add origin pinning without a decision.

Separate claim freshness from path-dependent forwarding. A wildcard's own
hash may select a different neighbor from a requested descendant. Likewise,
a local view may prefer a down-only route while an upstream export view must
retain a longer LAN alternative. Define feasibility and authority for those
views, including a partitioned local mesh reaching itself through the CDN.

Keep identity verification, discovery, transport, route selection, and reader
resume as separate responsibilities in the shared model. No P2P-specific
routing stack, cluster-wide identity folding, or trust in an arbitrary asserted
node ID. Authenticated namespaces still bound all forwarded claims.

Record assumptions, counterexamples, and the accepted contract in routes.md
and handoff.md. Use RFC 8966 sections 2.7 and 2.8 for multi-origin/overlapping
prefix limitations, and RFC 9234's export-policy treatment as a reference,
not a mandate to implement BGP roles. Fixtures must run in existing CI.

## Related

- [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) - claim freshness and reachability lifetimes
- [Peer grants](/quest/m1/p2p/peer-grant.md) - shared identity with session-bound proof, not replayable bearer identity
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - economic aggregation remains separate
