# m1: the dev line

## Goal

Everything that must land on `dev` before it merges to `main`: the breaking
API and wire changes (the announce and wildcard surface, error codes, the
allocator mirrors, the bindings), the merge gates (the archive catalog and
store, the monotonic timeline, wildcard advertisements), and the merge
itself.

## Plan

Branch a quest from `dev` when it breaks a published API or wire. A quest
stays here only if it breaks a published API or wire, or gates the merge;
[Merge dev](/quest/m1/merge-dev.md) names the gates. Work that is identical
on `main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The 2026-09-12 grooming applied that rule to every
quest here and merged main into dev.

## Quests

- [Archive catalog](/quest/m1/archive-catalog.md) - one root `archive` entry subsumes `timeline` and names the timeline track, replay path, store URL, and format version
- [Archive store](/quest/m1/archive-store.md) - `moq-archive` puts, gets, lists, and deletes the versioned objects over `object_store`
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a marker group of one empty frame declares a break and moves the live edge; producers refuse a rewind; consumers jump the playhead on an unproven hole and drop rewind detection
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - every native binding, Dart included, creates unadvertised, announces from the broadcast, and takes a path Pattern in `dynamic(pattern, route)`
- [JS announce](/quest/m1/js-announce.md) - js/net drops `publish()` and `RouteProvider` for `createBroadcast`, `announce(route)`, and a `dynamic(pattern, route)` handle
- [Advertise](/quest/m1/wildcard-advertise.md) - moq-net encodes, forwards, and authorizes wildcard advertisements, so `dynamic(pattern, route)` takes a pattern before the announce API is published
- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - moq-net: a route through an anonymous hop ranks below every identified route at any cost, and hop 0 travels the chain to say so
- [Group overflow](/quest/m1/group-overflow-abort.md) - an open group past its budget aborts for every reader with GROUP_TOO_LARGE, and head eviction is deleted
- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - one cloneable refcounted `Connection` mirroring `moq_tokio::Connection`; close releases a handle
- [Close classification](/quest/m1/js-close-classification.md) - a browser consumer tells a requested end from a fault, so the media harness fails on real errors during a transition
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - protocol error codes cross moq-ffi and C as a scope, code, and kind instead of a message string
- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - js/net mirrors the send-side bandwidth allocator so each publisher encodes against its own share
- [Binding rate control](/quest/m1/binding-rate-control.md) - the bindings mirror the allocator and reservation, so a non-Rust publisher follows its bandwidth share
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - passthrough imports claim their peak-hold catalog bitrate on the allocator so a co-resident encoder targets what is left
- [HLS 404](/quest/m1/hls-cache-miss-codes.md) - a relay miss and a disconnected publisher answer 404 over moq-lite; IETF upstreams stay 500
- [HLS sibling restart](/quest/m1/hls-sibling-epoch-identity.md) - a replaced sibling publisher restarts its rendition instead of serving stale rows
- [A/V clock](/quest/m1/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Cluster construction](/quest/m1/cluster-construction.md) - construct one stable origin after its cache settings are known, deleting the rebuilding builder
- [LAN discovery app id](/quest/m1/lan-app.md) - every advertisement names an application as a DNS-SD subtype bound into the proofs, so unrelated apps on one network never meet
- [One LAN mesh](/quest/m1/lan-mesh.md) - moq-cli drives the relay's Cluster, LAN peers authenticate by mDNS credential, and the two binaries mesh with each other
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [Transport feature](/quest/m1/tokio-transport-feature.md) - moq-tokio has one `_transport` gate and its backend-less build passes `-D warnings`
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
