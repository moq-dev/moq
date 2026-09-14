# m1: the dev line

## Goal

Work intended to land on `dev` before it merges to `main`, or be explicitly
deferred by the maintainer: the breaking
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

- [Auth server](/quest/m1/auth/README.md) - one JSON request per connection through `moq-auth`, `--auth-url` or `--auth-public` as the whole relay configuration, `moq auth serve` carrying the old policy, mTLS a fact and never a grant, moq-token retired
- [Worker ownership](/quest/m1/api-worker-ownership.md) - consuming split ownership retains sockets and joins the group before release
- [Origin scope API](/quest/m1/api-origin-pattern-scopes.md) - typed scope grants preserve current prefixes and refuse unsupported patterns
- [Cluster peer API](/quest/m1/api-cluster-peer-config.md) - typed peer configuration supports current symmetric policy
- [Broadcast clock](/quest/m1/broadcast-clock.md) - replace archive wall with one fixed catalog-root clock shared by every track
- [Publisher finish borrows](/quest/m1/api-finish-borrow.md) - finish borrows the handle so abort can still run after a clean end
- [External API proof](/quest/m1/api-release-proof.md) - packaged callers exercise real moq.pro use cases and record each audit finding's disposition
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a marker group of one empty frame declares a break and moves the live edge; producers refuse a rewind; consumers jump the playhead on an unproven hole and drop rewind detection
- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - moq-net: a route through an anonymous hop ranks below every identified route at any cost, and hop 0 travels the chain to say so
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
