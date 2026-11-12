# [S] Docs state the frozen moq-transport scope

## Goal

The docs that describe moq-transport support (`doc/concept/`,
`drafts/draft-lcurley-moq-ietf.md` if it lists the extensions, and the interop
matrix in [interop audit](/quest/m3/audit-interop.md)) state the frozen scope:
our moq-transport support covers what a third-party implementation needs to
exchange live media with ours, and stops there: SETUP, the standard
AUTHORIZATION TOKEN, live PUBLISH_NAMESPACE / SUBSCRIBE / PUBLISH, joining
FETCH, and relay forwarding between moq-transport and moq-lite sessions,
including across our relays, plus the extensions a third party speaks. The
code deletions are done; this is the docs work that remains.

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
- Kept (decided 2026-11-10, maintainer, in
  [moq-dev/moq#8445](https://github.com/moq-dev/moq/pull/8445)): moq-net's
  listing of a moq-transport peer's tracks with SUBSCRIBE_TRACKS, since
  Cloudflare's relay (cloudflare/moq-rs) answers it; without it we could not
  list tracks on that relay. Receiving SUBSCRIBE_TRACKS stays refused.
- Kept, as rejected in the same interview: lite-07's `ietf` object extension
  and moq-lite 01 to 04.
- Not here: `ietf/flate.rs`; compression removal has its own work.
- Narrowed 2026-11-12 (maintainer, `/quest-import` of
  [moq-dev/moq#8577](https://github.com/moq-dev/moq/issues/8577)): a Rust
  publisher may answer a multi-group FETCH whose every group is cached or
  proven never produced, using only draft-defined markers (Prior Group ID Gap
  in, End of Non-Existent Range out). [Range FETCH over known
  holes](/quest/m4/ietf-range-fetch.md) owns it; until it lands, the docs
  state joining FETCH only.

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

Sized S as a standalone quest (decided 2026-11-11, maintainer, in
[moq-dev/moq#8445](https://github.com/moq-dev/moq/pull/8445), which deleted
the last child; rejected: finishing the docs there, XS). AGENTS.md's IETF rule
is reworded only if the maintainer asks.

## Related

- [Code audit](/quest/m3/audit-code.md) - owns duplication inside the IETF code that remains
