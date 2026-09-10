# m1: the dev line

## Goal

Everything that must land on `dev` before it merges to `main`: the breaking
API and wire changes (the announce and wildcard surface, error codes, the
allocator mirrors, the bindings), the merge gates (the archive line, because
moq.pro needs archive-backed recording on the release that `dev` produces),
and the merge itself.

## Plan

Branch a quest from `dev` when it breaks a published API or wire. A merge
gate that lands on `main` (the duration marker, the additive Resolve and
Demand halves of the wildcard line) branches from `main` and ranks here only
because the merge waits on it. A quest stays here only if it breaks a
published API or wire, or gates the merge. Work that is identical on
`main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The 2026-09-09 grooming reconciled every quest here
with the dev tree.

## Quests

- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; the whole line gates the dev merge
- [Duration marker](/quest/m1/duration-marker.md) - every video group ends with an empty frame at its exclusive end; audio loses its end marker; lands on main but gates gap-discontinuity
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - a hole in the delivered group sequence resets the decoder unless the boundary is contiguous within 1 ms; empty groups stop meaning anything
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - producers refuse a group below the live edge and consumers drop rewind detection
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - every native binding, Dart included, creates unadvertised, announces from the broadcast, and takes a path Pattern in `dynamic(pattern, route)`
- [JS announce](/quest/m1/js-announce.md) - js/net drops `publish()` and `RouteProvider` for `createBroadcast`, `announce(route)`, and a `dynamic(pattern, route)` handle
- [Wildcard](/quest/m1/wildcard/README.md) - a service advertises a path pattern priced at its start-up cost; Advertise gates the merge, Resolve and Demand are additive
- [Route cold cost](/quest/m1/route-cold-cost.md) - MoqRoute carries warm and cold, so an observed route re-announces intact
- [#3060](/quest/m1/3060-moq-net-ban-hop-id-0-from-hop-chains.md) - a hop chain names real hops only; Hop ID 0 stays the absence marker
- [Group overflow](/quest/m1/group-overflow-abort.md) - an open group past its budget aborts for every reader with GROUP_TOO_LARGE, and head eviction is deleted
- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - one cloneable refcounted `Connection` mirroring `moq_tokio::Connection`; close releases a handle
- [Pooled leak control](/quest/m1/js-leak-pooled-connection.md) - the media harness catches an undetached player now that every player on a relay URL shares one transport
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - protocol error codes cross moq-ffi and C as a scope, code, and kind instead of a message string
- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - js/net mirrors the send-side bandwidth allocator so each publisher encodes against its own share
- [Binding rate control](/quest/m1/binding-rate-control.md) - the bindings mirror the allocator and reservation, so a non-Rust publisher follows its bandwidth share
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - passthrough imports claim their peak-hold catalog bitrate on the allocator so a co-resident encoder targets what is left
- [#2815](/quest/m1/2815-lift-adaptive-stage-refusal.md) - moq-cli accepts several adaptive import stages on one connection now that the allocator divides the estimate
- [HLS 404](/quest/m1/hls-cache-miss-codes.md) - a relay miss and a disconnected publisher answer 404 over moq-lite; IETF upstreams stay 500
- [HLS sibling restart](/quest/m1/hls-sibling-epoch-identity.md) - a replaced sibling publisher restarts its rendition instead of serving stale rows
- [A/V clock](/quest/m1/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Config provenance](/quest/m1/config-provenance.md) - the merge records which source set a value, so an empty TOML list survives the environment and env outranks the file
- [Cluster construction](/quest/m1/cluster-construction.md) - construct one stable origin after its cache settings are known, deleting the rebuilding builder
- [#3046](/quest/m1/3046-fold-moq-token-into-moq-token-via-a-usage-executable-view.md) - retire the standalone moq-token binary after one deprecation release; `moq token` is the only spelling
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - libmoq serves tracks on demand and accepts sessions, the two moq-ffi calls C still lacks
- [Transport feature](/quest/m1/tokio-transport-feature.md) - moq-tokio has one `_transport` gate and its backend-less build passes `-D warnings`
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
