# moq-transport is a compatibility layer again

## Goal

Our moq-transport support covers what a third-party implementation needs to
exchange live media with ours, and stops there: SETUP, the standard
AUTHORIZATION TOKEN, live PUBLISH_NAMESPACE / SUBSCRIBE / PUBLISH, joining
FETCH, and relay forwarding between moq-transport and moq-lite sessions,
including across our relays, plus the extensions a third party speaks. The
IETF-only features nobody third-party uses are deleted from Rust, JS, and the
drafts.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ freeze the IETF scope (rejected: keep growing IETF features to
parity with lite).

AGENTS.md calls moq-transport "a low-effort compatibility layer", but Rust
`ietf/` is 62.3k lines (28.3k logic) against `lite/`'s 40.4k (21.0k logic),
and both `ietf/publisher.rs` and `ietf/subscriber.rs` grew by thousands of
lines since 2026-10-23. Since August, the Rust IETF code took 465 commits
against lite's 429.

Settled with this epic:

- Deleted from m5 by this PR, as work the freeze rules out:
  moq-transport hole markers and moq-transport cluster peers.
- Kept: moq-transport drafts 18, 20, and 22 (draft 20 restored on purpose in
  [moq-dev/moq#7190](https://github.com/moq-dev/moq/pull/7190)), and the
  relay paths moq.pro's fleet smoke runs: cross-relay moq-transport
  subscribes and a draft-18 session to a third-party relay.
- Kept, as rejected in the same interview: lite-07's `ietf` object extension
  and moq-lite 01 to 04.
- Not here: `ietf/flate.rs`; compression removal has its own work.

Our moq-transport extensions, decided 2026-11-10 (maintainer, on
[moq-dev/moq#8444](https://github.com/moq-dev/moq/pull/8444)):
✅ keep MoQ Cluster (HOP_ID, RELAY_COST, HOP_PATH, ROUTE_COST), Solicit,
Hidden, and Active Count, and delete only the in-band AUTH exchange (rejected:
delete all five). OpenMOQ's moqx relay implements draft-lcurley-moq-cluster-01
over moq-transport draft 18 and turns it on by default, so a third party does
speak cluster, and the other three cost about 780 dedicated lines between
them. AUTH cost about 2k dedicated lines plus wiring and no third party uses
it; a third party authenticates with the standard AUTHORIZATION TOKEN, which
stays. In-band grants and token renewal are moq-lite-07 only.

This epic's own work, after its children: the docs that describe
moq-transport support (`doc/concept/`, `drafts/draft-lcurley-moq-ietf.md` if
it lists the extensions, and the interop matrix in
[interop audit](/quest/m3/audit-interop.md)) state the frozen scope, and
AGENTS.md's IETF rule is reworded only if the maintainer asks.

## Related

- [Simplification audit](/quest/m3/audit-simplify.md) - owns duplication inside the IETF code that remains
