# [S] A regrant re-checks only the routes it can change

## Goal

When a peer's grant changes (a new `AUTH_OK`), a moq-lite announce stream's cost to apply it no longer grows as live routes times grant members per stream. A benchmark swept over announce streams, live routes, and grant size shows the per-stream regrant cost bounded by what the grant change can affect. Check whether `@moq/net`'s publisher re-matches its announcements the same way on a regrant, and bound it there too if so.

## Plan

Found by [moq-dev/moq#7536](https://github.com/moq-dev/moq/pull/7536), which swept the worst known peer grant shapes over `AUTH_OK` size and left this scope question to a follow-up (its decision 2, recommended there because it is route fan-out rather than grant algebra). On a regrant, each announce stream's `regrant` (`AnnounceRun::regrant` in `rs/moq-net/src/lite/publisher.rs`, line 1207 at 4bf501b535):

- filters every live route through `permitted` (a match against the new permit) to find what the new grant revokes, and
- when the grant grew, takes a fresh `origin.announced()` snapshot of every route under its prefix and matches each one it doesn't hold.

So one `AUTH_OK` costs routes × members per stream, times K announce streams, a product [peer grant bound](/quest/m5/peer-grant-bound.md), which bounds decoding and comparing the grant itself, leaves alone.

Since [moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251) a grant is a sorted set of prefixes (`Prefixes` in `moq-pattern`), and matching one path costs a binary search per segment of the path, so the per-route factor is log members rather than members. The walk over every live route remains, and is what this quest bounds.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): its own quest beside the grant-algebra bound.

Re-scoped 2026-11-12 (/quest-audit): [Each side says everything about routing on one stream](https://github.com/moq-dev/moq/pull/8835) gives a lite-07 side one Announce Stream and turns a token-scope change into an edit of its interest set, so on lite-07 the factor K becomes one and the per-stream `regrant` this plan targets remains only for lite-06 and older. Re-measure the regrant (or interest-set edit) cost on lite-07 with the benches below, and apply the steps that follow to whichever path still pays routes × members. If only the older versions still pay it, say so in the PR and leave them.

As shipped by [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987), lite-07 regrants in `Routed::poll_permit` (`rs/moq-net/src/lite/publisher/routed.rs`): a routing peer's grant must stay whole or the session closes, and on a claims-only session every claim told or live is re-synced against the new permit, so lite-07 still pays its claims times grant members once per session. The `AnnounceRun::regrant` path above now serves lite-06 and older only.

- Derive the change once per `AUTH_OK`, not per stream: the members added and removed between the old and new grant. Each stream then re-checks only the routes under a changed member, using the origin's prefix structure to find them instead of walking every route. A member is a prefix (`Permit` in `rs/moq-net/src/auth.rs` holds `Prefixes`), so the routes it can change are exactly those at, beneath, or above it. Test removed members, members nested in old ones, and old and new grants that overlap.
- A grown grant needs only routes under the added members, not a whole-origin snapshot per stream.
- Keep today's semantics: a revoked route retracts, a newly covered route advertises, and a route both grants cover is untouched.
- Bench over announce streams, live routes, and grant members, all swept; report before/after.

Public API: none. Wire: none.

## Related

- [Bound peer grant processing by design](/quest/m5/peer-grant-bound.md) - the cost of decoding and comparing one grant, which this builds on
- [Cap the legacy announce interest streams](/quest/m4/announce-interest-memory.md) - the per-stream route state that this walk reads on lite-06 and older
