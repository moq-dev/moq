# m1: the dev line

## Goal

Everything that must land on `dev` before it merges to `main`: the breaking
API and wire changes (the announce and wildcard surface, error codes, the
allocator mirrors, the bindings), the merge gates (the monotonic timeline),
and the merge itself.

## Plan

Branch a quest from `dev` when it breaks a published API or wire. A quest
stays here only if it breaks a published API or wire, or gates the merge;
[Merge dev](/quest/m1/merge-dev.md) names the gates. Work that is identical
on `main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The 2026-09-12 grooming applied that rule to every
quest here and merged main into dev. The auth API line is here for its
request-side break (`mtls=<identity>` and the now-required fields) and ranks
first because moq.pro adopts the release only once that contract is settled;
it is priority, not a merge gate, and [Merge dev](/quest/m1/merge-dev.md)
does not require it.

## Quests

- [Auth API](/quest/m1/auth-api/README.md) - the endpoint contract moq.pro adopts: versioned grants, named mTLS peers, re-checks that move a tier and resize a scope, one open question planned first
- [Relay embedding](/quest/m1/api-relay-embedding.md) - custom routes retain the owner of listeners, workers, and shutdown
- [FFI frame cursor](/quest/m1/api-ffi-frame-cursor.md) - empty groups and cancelled reads do not become false EOF or lost frames
- [Subscription bounds](/quest/m1/api-subscription-bounds.md) - local and requested ranges use consistent exclusive ends
- [Publisher finish borrows](/quest/m1/api-finish-borrow.md) - finish borrows the handle so abort can still run after a clean end
- [External API proof](/quest/m1/api-release-proof.md) - packaged callers exercise real moq.pro use cases and record each audit finding's disposition
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a marker group of one empty frame declares a break and moves the live edge; producers refuse a rewind; consumers jump the playhead on an unproven hole and drop rewind detection
- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - moq-net: a route through an anonymous hop ranks below every identified route at any cost, and hop 0 travels the chain to say so
- [A/V clock](/quest/m1/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [Wildcard docs](/quest/m1/wildcard-docs.md) - path patterns and wildcard advertisements are documented on every surface the dev PRs touched before the release ships them
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
