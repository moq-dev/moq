# [M] Relay reachability

## Goal

Relays maintain the validated policy's reachability and readiness evidence over
the cluster stream, while announcements still flood. The internal HTTP listener
shows that evidence, its session ownership, and why a route is ineligible for
pruning. Operators can distinguish a proposed path from a usable retained route.

## Plan

Implement the [policy](/quest/m1/announce-tree/policy.md) without adding an
independent routing order. For path-vector reachability, reuse hop-loop checks
and accumulated link prices. Select each peer's advertised route after excluding
that peer, so an alternate can be sent back when the global best came from it.

Define prefix-independent comparison keys explicitly. Preserve alternatives
needed by the policy before any prefix-dependent hash; a single representative
chain cannot certify every broadcast's backup. A reachability advert is not
proof that its sender will advertise a particular source for a prefix.

Invalidate evidence on link loss, withdrawal, cost/drain changes, session
replacement, and relay restart. Restored links become eligible through the
policy's explicit synchronization, not a fixed stability delay. If ordinary
relay reachability cannot represent a route's scope or identity, leave that
route in flood mode. Do not advertise unsupported node-protection claims.

Expose proposed and ready state separately. Exercise a weighted ring with a
chord, equal-cost choices, zero-cost siblings, parallel sessions, mixed versions,
flaps, and restarted random hop IDs in the simulator. Check actual per-peer
alternates and readiness transitions, not just shortest-path distances.

## Required

- [Cluster stream](/quest/m1/announce-tree/cluster-stream.md) - authenticated control transport and lifecycle
