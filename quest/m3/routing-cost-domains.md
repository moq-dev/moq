# [M] Design administrative routing cost domains

## Goal

Settle how independently operated MoQ networks exchange reachability without
adding incomparable costs, at the boundaries between operators on the one node id space
[Cluster routing](/quest/m1/cluster-routing/README.md) routes over. Cost inside
one operator's network is cluster-routing's. This includes whether a boundary
may fold a large customer cluster's nodes into one advertised node, and how
that keeps loop safety. Produce a
reviewed design and scoped implementation quests, not a protocol
implementation. Cloudflare, moq.pro, and self-hosted relays can retain their
own business policy; no RTT/loss-driven repricing or automatic performance
failover is authorized by this work.

## Plan

Costs reflect SKU, directional egress economics, provider affinity, and marginal
transfer savings. A hosting provider is not necessarily a policy domain:
moq.pro can coordinate one model across its OVH and Linode nodes. Domain
membership must come from an operator-controlled trust boundary, not a peer's
unverified assertion.

Investigate a shared cost model within a coordinating domain and explicit
import/export policy at its boundaries. A remote number is not comparable to
ours merely because both are integers. Prefer local policy and existing
mechanisms where sufficient; do not prescribe a new domain identifier, cost
vector, or exchange-rate scheme before demonstrating its necessity. Separate
reachability and loop prevention from optional economic hints. Business logic
stays outside the base media protocol; justify any minimal cluster extension
against a configuration-only alternative.

Use these references to evaluate the design:

- [BGP, RFC 4271](https://www.rfc-editor.org/rfc/rfc4271.html): LOCAL_PREF,
  import/export policy, AS_PATH, and MED's same-neighbor comparison scope.
- [MED considerations, RFC 4451](https://www.rfc-editor.org/rfc/rfc4451.html):
  scoped comparisons and oscillation risks.
- [Large Communities, RFC 8092](https://www.rfc-editor.org/rfc/rfc8092.html):
  namespaced policy signals rather than universally comparable prices.
- [Inter-domain TE, RFC 7926](https://www.rfc-editor.org/rfc/rfc7926.html):
  abstract reachability across domains without exposing complete internals.
- [Gao and Rexford](https://www.cs.princeton.edu/~jrex/papers/ton01-stable.pdf):
  conditions for policy convergence; loop prevention alone does not prove it.

The design must work through examples of two operators using different scales,
multiple entrances to one domain, asymmetric charges, mixed-provider nodes in
one domain, unknown or untrusted peers, and a route leaving and re-entering a
domain. Preserve publisher identity and loop safety across any metric rewrite.
State tradeoffs, migration and
mixed-version behavior, and the limits of any convergence claim.

Reconcile with cluster-routing's configured link costs rather than creating
a second peer-policy mechanism. Completion is a documented decision,
worked counterexamples or model checks, and independently completable follow-up
quests. Open wire/API choices belong to this design exercise.

## Required

- [Cluster routing](/quest/m1/cluster-routing/README.md) - the route layer this designs policy on

## Related

- [#3769](https://github.com/moq-dev/moq/pull/3769) - measurement-based pricing
  prompted the separation of measurement, operator policy, and protocol
