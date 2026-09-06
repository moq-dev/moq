# m1: the dev line

## Goal

Everything that lands on the dev branch or with its merge to main: the
thread-per-core runtime (moq-uring, moq-tokio, quiche), the net model and
allocator follow-ups, breaking bindings work, and the archive line that gates
the merge itself, because moq.pro needs archive-backed recording on the release
that `dev` produces before it can adopt it.

## Plan

Branch these quests from dev, not main. Several were rescoped during the
2026-08 grooming because dev already moved under them; reconcile each plan
with the current dev tree before starting.

## Quests

- [Compressed delta regression](/quest/m1/json-compressed-delta-test-timeout.md) - diagnose the encoded-size test timeout under load

- [Stream sessions](/quest/m1/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [Perf](/quest/m1/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model: copies, locks, clock reads, allocations, syscalls
- [#2296](/quest/m1/2296-moq-native-bring-the-quiche-backend-to-quinn-noq-feature.md) - moq-tokio: bring the quiche backend to quinn/noq feature parity
- [#2924](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - moq-relay: TLS rotation is not atomic across thread-per-core QUIC workers
- [#2964](/quest/m1/2964-quic-workers-dropping-one-split-server-resizes-the.md) - QUIC workers: dropping one split() Server resizes the reuseport group
- [#2979](/quest/m1/2979-moq-tokio-does-not-compile-with-no-default-features-and.md) - moq-tokio compiles with any subset of transport features, and nightly checks it per crate
- [#2853](/quest/m1/2853-quiche-with-a-pinned-source-port-can-dial-only-a-broken.md) - quiche with a pinned source port can dial only a broken IPv4 address
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - a hole in the delivered group sequence is the discontinuity unless the boundary proves continuity; no marker has to arrive
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a track's timestamps never fall below its live edge; publishers declare a discontinuity and continue forward, consumers stop detecting rewinds
- [Group overflow](/quest/m1/group-overflow-abort.md) - an oversized open group aborts for every reader instead of shedding its head
- [#2895](/quest/m1/2895-add-an-atomic-readiness-gate-for-origin-broadcasts.md) - Add an atomic readiness gate for Origin broadcasts
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - net: coalesce dynamic tracks and preserve sequences across replacements
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - every native binding exposes create_broadcast, announce/unannounce on the broadcast, and dynamic(prefix, route) with one meaning
- [JS announce](/quest/m1/js-announce.md) - js/net gets createBroadcast, a broadcast-owned announcement, and the dynamic handle
- [Dart announce](/quest/m1/dart-announce.md) - the Dart wrapper mirrors the same three operations once dev merges
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; gates the dev merge
- [Playable](/quest/m1/hls-playable.md) - a 24/7 broadcast never becomes permanently unplayable over HLS
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - Follow the bandwidth grant in moq-audio instead of holding a fixed reservation
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - Passthrough imports reserve no bandwidth, so a co-resident encoder over-targets
- [Ladder](/quest/m1/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [Plan: binding rate control](/quest/m1/plan-binding-rate-control.md) - settle how a non-Rust publisher follows the send estimate before wiring five bindings
- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - js/net mirrors the send-side bandwidth allocator so each publisher encodes against its own share
- [#3000](/quest/m1/3000-track-teardown-on-poll-unused-is-not-atomic-against-a.md) - Track teardown on poll_unused is not atomic against a consumer reattaching
- [IETF stream types](/quest/m1/ietf-uni-stream-types.md) - accept padding and close sessions for genuinely unknown uni-stream types
- [HLS cache misses](/quest/m1/hls-cache-miss-codes.md) - moq-hls: a segment the relay dropped is served as a 500, because the miss is matched against a table the wire stopped using
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - Preserve structured protocol error codes across FFI and C bindings
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - js/net: remaining capability gaps vs rs/moq-net (SETUP role, finish_at and final sequence, range controls, typed errors)
- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - Collapse Reload and Shared into one Connection class
- [HLS dead publisher](/quest/m1/hls-closed-publisher-500.md) - a segment whose publisher disconnected answers 500 instead of 404
- [HLS sibling identity](/quest/m1/hls-sibling-epoch-identity.md) - validate sibling media against the epoch described by its catalog
- [#2075](/quest/m1/2075-mirror-catalog-reservation-gating-in-moq-hang-js-hang.md) - Mirror catalog reservation gating in @moq/hang (js/hang)
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - Video rotation metadata not propagated from mobile camera publish to watch renderer
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: video decoder captures the rewind generation at output time, not submit time
- [Config provenance](/quest/m1/config-provenance.md) - the merge records which source set a value, so TOML survives CLI defaults and empty lists, and env outranks the file
- [Cluster construction](/quest/m1/cluster-construction.md) - construct one stable origin after its cache settings are known, deleting the rebuilding builder
- [#3046](/quest/m1/3046-fold-moq-token-into-moq-token-via-a-usage-executable-view.md) - Fold moq-token into moq token via a Usage executable view
- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench: every README example fails to parse, and cumulative latency percentiles cannot be windowed to steady state
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - libmoq: C ABI catch-up with the moq-ffi surface
- [Plan: route cold cost](/quest/m1/plan-route-cold-cost.md) - settle how a route's cold cost crosses the bindings without being rewritten on the way back
- [#3060](/quest/m1/3060-moq-net-ban-hop-id-0-from-hop-chains.md) - moq-net: ban Hop ID 0 from hop chains
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
