# [S] A regrant re-checks only the routes it can change

## Goal

When a peer's grant changes (a new `AUTH_OK`), a moq-lite announce stream's cost to apply it no longer grows as live routes times grant members per stream. A benchmark swept over announce streams, live routes, and grant size shows the per-stream regrant cost bounded by what the grant change can affect. Check whether `@moq/net`'s publisher re-matches its announcements the same way on a regrant, and bound it there too if so.

## Plan

Found by [moq-dev/moq#7536](https://github.com/moq-dev/moq/pull/7536), which swept the worst known peer grant shapes over `AUTH_OK` size and left this scope question to a follow-up (its decision 2, recommended there because it is route fan-out rather than grant algebra). On a regrant, each announce stream's `regrant` (`AnnounceRun` in `rs/moq-net/src/lite/publisher.rs`, about line 1045):

- filters every live route through `permitted` (a match against every member of the new permit) to find what the new grant revokes, and
- when the grant grew, takes a fresh `origin.announced()` snapshot of every route under its prefix and matches each one it doesn't hold.

So one `AUTH_OK` costs routes × members per stream, times K announce streams, a product none of [peer grant bound](/quest/m2/peer-grant-bound.md)'s options remove.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, its own quest beside the grant-algebra bound.

- Derive the change once per `AUTH_OK`, not per stream: the members added and removed between the old and new grant. Each stream then re-checks only the routes under a changed member's prefix, using the origin's prefix structure to find them instead of walking every route. A member's prefix here is its literal segments before its first wildcard (`*` or `**`), since grant members are `Patterns` (`Permit` in `rs/moq-net/src/auth.rs`); a member that starts with a wildcard has no literal prefix and re-checks every route under the stream's prefix, as today. Bench that shape on its own row, and test wildcard members, removed members, and old and new grants that overlap.
- A grown grant needs only routes under the added members, not a whole-origin snapshot per stream.
- Keep today's semantics: a revoked route retracts, a newly covered route advertises, and a route both grants cover is untouched.
- Bench with #7536's grant shapes over announce streams and live routes, both swept; report before/after.

Public API: none. Wire: none.

## Related

- [Bound peer grant processing by design](/quest/m2/peer-grant-bound.md) - may change the received-grant type this compares against; whichever lands second adapts
- [Announce interest streams share one route table](/quest/m2/announce-interest-memory.md) - the per-stream route state that this walk reads
