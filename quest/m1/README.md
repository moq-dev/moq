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

- [Worker metrics](/quest/m1/uring-metrics.md) - per-worker io_uring counters at `/metrics`, so the runtime's own health is visible
- [Stream sessions](/quest/m1/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [qlog](/quest/m1/uring-qlog.md) - io_uring workers write qlog traces instead of refusing the setting
- [Perf](/quest/m1/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model: copies, locks, clock reads, allocations, syscalls
- [#2296](/quest/m1/2296-moq-native-bring-the-quiche-backend-to-quinn-noq-feature.md) - moq-native: bring the quiche backend to quinn/noq feature parity
- [#3092](/quest/m1/3092-moq-sock-make-the-reuseport-groups-invariants.md) - moq-sock: make the reuseport group's invariants unrepresentable, not documented
- [#2924](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - moq-relay: TLS rotation is not atomic across thread-per-core QUIC workers
- [#2964](/quest/m1/2964-quic-workers-dropping-one-split-server-resizes-the.md) - QUIC workers: dropping one split() Server resizes the reuseport group
- [#2979](/quest/m1/2979-moq-tokio-does-not-compile-with-no-default-features-and.md) - moq-tokio compiles with any subset of transport features, and nightly checks it per crate
- [#2980](/quest/m1/2980-moq-relay-nothing-guards-the-socket-capturing-acceptor.md) - moq-relay: nothing guards the socket-capturing acceptor being installed on a listener
- [#2853](/quest/m1/2853-quiche-with-a-pinned-source-port-can-dial-only-a-broken.md) - quiche with a pinned source port can dial only a broken IPv4 address
- [#2624](/quest/m1/2624-moq-native-goaway-redirect-guard-classifies-hosts-by-name.md) - moq-native: GOAWAY redirect guard classifies hosts by name, not by resolved address
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - Retention should reclaim idle open groups now that expiry is timestamp-only
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - a hole in the delivered group sequence is the discontinuity unless the boundary proves continuity; no marker has to arrive
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a track's timestamps never fall below its live edge; publishers declare a discontinuity and continue forward, consumers stop detecting rewinds
- [Group overflow](/quest/m1/group-overflow-abort.md) - an oversized open group aborts for every reader instead of shedding its head
- [#2895](/quest/m1/2895-add-an-atomic-readiness-gate-for-origin-broadcasts.md) - Add an atomic readiness gate for Origin broadcasts
- [#2985](/quest/m1/2985-js-net-path-keyed-publisher-state-goes-stale-when-a.md) - js/net: path-keyed publisher state goes stale when a broadcast is replaced
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - net: coalesce dynamic tracks and preserve sequences across replacements
- [Announce handle](/quest/m1/announce-handle.md) - announce(prefix, route) advertises and serves requests; create_broadcast plus set_announce publishes
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - every native binding exposes create_broadcast, set_announce, and announce with one meaning
- [JS announce](/quest/m1/js-announce.md) - js/net gets createBroadcast, an announce flag, and the announce handle
- [Dart announce](/quest/m1/dart-announce.md) - the Dart wrapper mirrors the same three operations once dev merges
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; gates the dev merge
- [Playable](/quest/m1/hls-playable.md) - a 24/7 broadcast never becomes permanently unplayable over HLS
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - Follow the bandwidth grant in moq-audio instead of holding a fixed reservation
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - Passthrough imports reserve no bandwidth, so a co-resident encoder over-targets
- [Ladder](/quest/m1/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [Plan: binding rate control](/quest/m1/plan-binding-rate-control.md) - settle how a non-Rust publisher follows the send estimate before wiring five bindings
- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - js/net mirrors the send-side bandwidth allocator so each publisher encodes against its own share
- [#3000](/quest/m1/3000-track-teardown-on-poll-unused-is-not-atomic-against-a.md) - Track teardown on poll_unused is not atomic against a consumer reattaching
- [JS stream codes](/quest/m1/js-net-stream-error-codes.md) - js/net: a locally raised group error reaches the wire as INTERNAL_ERROR
- [#3002](/quest/m1/3002-no-test-drives-a-late-group-through-the-ietf-dispatch-loop.md) - No test drives a late group through the IETF dispatch loop
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - Preserve structured protocol error codes across FFI and C bindings
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - js/net: remaining capability gaps vs rs/moq-net (SETUP role, finish_at and final sequence, range controls, typed errors)
- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - Collapse Reload and Shared into one Connection class
- [#2870](/quest/m1/2870-moq-hls-a-named-sibling-rendition-is-pinned-too-late-to.md) - moq-hls: a named sibling rendition is pinned too late to survive a same-path republish
- [#2075](/quest/m1/2075-mirror-catalog-reservation-gating-in-moq-hang-js-hang.md) - Mirror catalog reservation gating in @moq/hang (js/hang)
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - Video rotation metadata not propagated from mobile camera publish to watch renderer
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: video decoder captures the rewind generation at output time, not submit time
- [Config provenance](/quest/m1/config-provenance.md) - the merge records which source set a value, so TOML survives CLI defaults and empty lists, and env outranks the file
- [Cluster construction](/quest/m1/cluster-construction.md) - construct one stable origin after its cache settings are known, deleting the rebuilding builder
- [#3046](/quest/m1/3046-fold-moq-token-into-moq-token-via-a-usage-executable-view.md) - Fold moq-token into moq token via a Usage executable view
- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench: every README example fails to parse, and cumulative latency percentiles cannot be windowed to steady state
- [#816](/quest/m1/816-expose-transportconfig.md) - QUIC flow-control windows on quic::Client and quic::Server, applied or refused per backend
- [#3188](/quest/m1/3188-make-every-blocking-go-operation-cancellable-with-context.md) - Make every blocking Go operation cancellable with context.Context
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [#3208](/quest/m1/3208-make-2-5-ms-opus-frame-durations-work-across-bindings.md) - Make 2.5 ms Opus frame durations work across bindings
- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - libmoq: C ABI catch-up with the moq-ffi surface
- [Plan: route cold cost](/quest/m1/plan-route-cold-cost.md) - settle how a route's cold cost crosses the bindings without being rewritten on the way back
- [#3060](/quest/m1/3060-moq-net-ban-hop-id-0-from-hop-chains.md) - moq-net: ban Hop ID 0 from hop chains
- [#2248](/quest/m1/2248-moq-mux-rebase-fmp4-export-timestamps-for-late-subscribers.md) - moq-mux: rebase fMP4 export timestamps for late subscribers
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
