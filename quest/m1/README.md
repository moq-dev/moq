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
[Merge dev](/quest/m1/merge-dev.md) names the gates, and [Release](/quest/m1/release.md)
names what gates the release after it. Work that is identical
on `main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The 2026-09-12 grooming applied that rule to every
quest here and merged main into dev. The auth API line is here for its
request-side break (`mtls=<identity>` and the now-required fields) and ranks
first because moq.pro adopts the release only once that contract is settled;
it is priority, not a merge gate, and [Merge dev](/quest/m1/merge-dev.md)
does not require it.

## Quests

- [Reserved codes](/quest/m1/lite-reserved-codes.md) - the four stream codes sent from the reserved range are registered in the draft's own range and round-trip in both languages
- [Worker ownership](/quest/m1/api-worker-ownership.md) - consuming split ownership retains sockets and joins the group before release
- [Origin scope API](/quest/m1/api-origin-pattern-scopes.md) - typed scope grants preserve current prefixes and refuse unsupported patterns
- [Cluster peer API](/quest/m1/api-cluster-peer-config.md) - typed peer configuration supports current symmetric policy
- [Broadcast clock](/quest/m1/broadcast-clock.md) - replace archive wall with one fixed catalog-root clock shared by every track
- [Publisher finish borrows](/quest/m1/api-finish-borrow.md) - finish borrows the handle so abort can still run after a clean end
- [Rate estimate names](/quest/m1/api-rate-estimate-names.md) - the send and receive estimates carry `estimated_*_rate` on the C ABI, every binding, and moqsink
- [Announce names](/quest/m1/api-announce-names.md) - one name per announce, request, and origin config concept in Rust, JS, moq-ffi, and C
- [moq-tokio names](/quest/m1/api-tokio-names.md) - moq-tokio's public names sit under their modules with no root compounds, adapter names, or forgettable close()
- [JSON config names](/quest/m1/api-json-binary-config-names.md) - `Config` means the codec options and `producer::Config` / `consumer::Config` the track pair in all four json and binary packages
- [JS names](/quest/m1/api-js-net-names.md) - @moq/net and @moq/pattern mirror Rust: `consume`, typed durations, one `readFrame()`, `InvalidPattern`
- [FFI units](/quest/m1/api-ffi-units-verbs.md) - every moq-ffi duration is microseconds and `publish()` / `consume()` pair with their setters
- [Deprecated sweep](/quest/m1/deprecated-sweep.md) - uncalled deprecated items, warn-then-ignore flags, and silent aliases are removed or become refusals before the release
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a marker group of one empty frame declares a break and moves the live edge; producers refuse a rewind; consumers jump the playhead on an unproven hole and drop rewind detection
- [Joining FETCH](/quest/m1/joining-fetch.md) - moq-net: every subscribe to a draft-14 to draft-19 relay joins at a group boundary, with contiguous history for an explicit start, via Largest Object plus a joining FETCH
- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - moq-net: a route through an anonymous hop ranks below every identified route at any cost, and hop 0 travels the chain to say so
- [Cluster -01](/quest/m1/cluster-01/README.md) - rs/moq-net and js/net speak the revised cluster extension (HOP_ID, REQUEST_UPDATE repricing) and -01 is published
- [Track demand](/quest/m1/libmoq-track-demand.md) - a C publisher sees used/unused per track and serves dynamic track and group requests, so an encoder runs only while someone watches
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
- [Release](/quest/m1/release.md) - the release moq.pro adopts: binding parity, an upgrade page, and a staging soak gate it rather than the merge
