# m1: the dev line

## Goal

Everything that lands on the dev branch or with its merge to main: the
thread-per-core runtime (moq-uring, moq-tokio, quiche), the net model and
allocator follow-ups, breaking bindings work, and the archive line that gates
the merge itself.

## Plan

Branch these quests from dev, not main. Several were rescoped during the
2026-08 grooming because dev already moved under them; reconcile each plan
with the current dev tree before starting.

## Quests

- [Worker metrics](/quest/m1/uring-metrics.md) - per-worker io_uring counters at `/metrics`, so the runtime's own health is visible
- [Stream sessions](/quest/m1/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [qlog](/quest/m1/uring-qlog.md) - io_uring workers write qlog traces instead of refusing the setting
- [#2296](/quest/m1/2296-moq-native-bring-the-quiche-backend-to-quinn-noq-feature.md) - moq-native: bring the quiche backend to quinn/noq feature parity
- [#3092](/quest/m1/3092-moq-sock-make-the-reuseport-groups-invariants.md) - moq-sock: make the reuseport group's invariants unrepresentable, not documented
- [#2924](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - moq-relay: TLS rotation is not atomic across thread-per-core QUIC workers
- [#2964](/quest/m1/2964-quic-workers-dropping-one-split-server-resizes-the.md) - QUIC workers: dropping one split() Server resizes the reuseport group
- [#2979](/quest/m1/2979-moq-tokio-does-not-compile-with-no-default-features-and.md) - moq-tokio does not compile with --no-default-features, and workspace feature unification hides it
- [#2980](/quest/m1/2980-moq-relay-nothing-guards-the-socket-capturing-acceptor.md) - moq-relay: nothing guards the socket-capturing acceptor being installed on a listener
- [#2853](/quest/m1/2853-quiche-with-a-pinned-source-port-can-dial-only-a-broken.md) - quiche with a pinned source port can dial only a broken IPv4 address
- [#2624](/quest/m1/2624-moq-native-goaway-redirect-guard-classifies-hosts-by-name.md) - moq-native: GOAWAY redirect guard classifies hosts by name, not by resolved address
- [#3122](/quest/m1/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()
- [#3129](/quest/m1/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring: write the WebTransport stream header at open time, so finish() never owes one
- [#3199](/quest/m1/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup
- [#3200](/quest/m1/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [#3201](/quest/m1/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - moq-uring: use SENDMSG_ZC for large UDP GSO trains
- [#3202](/quest/m1/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: use fixed-file slots for worker UDP sockets
- [#3204](/quest/m1/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [#3086](/quest/m1/3086-refactor-net-make-group-delivery-order-a-handle-and-move.md) - refactor(net): make group delivery order a handle, and move timestamp-based skipping into moq-net
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - Retention should reclaim idle open groups now that expiry is timestamp-only
- [#699](/quest/m1/699-better-prioritize-ties.md) - Better prioritize ties
- [#2895](/quest/m1/2895-add-an-atomic-readiness-gate-for-origin-broadcasts.md) - Add an atomic readiness gate for Origin broadcasts
- [#2985](/quest/m1/2985-js-net-path-keyed-publisher-state-goes-stale-when-a.md) - js/net: path-keyed publisher state goes stale when a broadcast is replaced
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - net: coalesce dynamic tracks and preserve sequences across replacements
- [#2708](/quest/m1/2708-origin-open-the-announce-interest-lazily-on-first-consumer.md) - origin: open the announce interest lazily, on first consumer
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; gates the dev merge
- [Playable](/quest/m1/hls-playable.md) - a 24/7 broadcast never becomes permanently unplayable over HLS
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - Follow the bandwidth grant in moq-audio instead of holding a fixed reservation
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - Passthrough imports reserve no bandwidth, so a co-resident encoder over-targets
- [Ladder](/quest/m1/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [#2857](/quest/m1/2857-bindings-cant-reach-encoder-rate-control-so-every-non.md) - Bindings can't reach encoder rate control, so every non-Rust publisher ignores congestion
- [#2709](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - per-broadcast bandwidth estimates and reservation
- [#3000](/quest/m1/3000-track-teardown-on-poll-unused-is-not-atomic-against-a.md) - Track teardown on poll_unused is not atomic against a consumer reattaching
- [#3001](/quest/m1/3001-ietf-stream-resets-send-moq-lite-error-codes-so-routine.md) - IETF stream resets send moq-lite error codes, so routine events read as INTERNAL_ERROR
- [#3002](/quest/m1/3002-no-test-drives-a-late-group-through-the-ietf-dispatch-loop.md) - No test drives a late group through the IETF dispatch loop
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - Preserve structured protocol error codes across FFI and C bindings
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - js/net: remaining capability gaps vs rs/moq-net (SETUP role, finish_at and final sequence, range controls, typed errors)
- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - Collapse Reload and Shared into one Connection class
- [#2714](/quest/m1/2714-per-subscriber-path-predicate-for-originconsumer-0-1-x.md) - Per-subscriber path predicate for OriginConsumer (0.1.x) / AnnounceConsumer (0.2.x)
- [#2870](/quest/m1/2870-moq-hls-a-named-sibling-rendition-is-pinned-too-late-to.md) - moq-hls: a named sibling rendition is pinned too late to survive a same-path republish
- [#2075](/quest/m1/2075-mirror-catalog-reservation-gating-in-moq-hang-js-hang.md) - Mirror catalog reservation gating in @moq/hang (js/hang)
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - Video rotation metadata not propagated from mobile camera publish to watch renderer
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: video decoder captures the rewind generation at output time, not submit time
- [#3221](/quest/m1/3221-config-stop-cli-defaults-from-clobbering-toml-values.md) - config: stop CLI defaults from clobbering TOML values
- [#3051](/quest/m1/3051-config-merge-empty-lists-lose-to-the-environment-and-the.md) - Config merge: empty lists lose to the environment, and the file outranks it
- [#3046](/quest/m1/3046-fold-moq-token-into-moq-token-via-a-usage-executable-view.md) - Fold moq-token into moq token via a Usage executable view
- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench: every README example fails to parse, and cumulative latency percentiles cannot be windowed to steady state
- [#816](/quest/m1/816-expose-transportconfig.md) - Expose TransportConfig
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - Align origin broadcast creation naming across language bindings
- [#3188](/quest/m1/3188-make-every-blocking-go-operation-cancellable-with-context.md) - Make every blocking Go operation cancellable with context.Context
- [#3208](/quest/m1/3208-make-2-5-ms-opus-frame-durations-work-across-bindings.md) - Make 2.5 ms Opus frame durations work across bindings
- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - libmoq: C ABI catch-up with the moq-ffi surface
- [#2933](/quest/m1/2933-moq-ffi-moqroute-round-trip-erases-a-routes-cold-cost.md) - moq-ffi: MoqRoute round-trip erases a route's cold cost
- [#3060](/quest/m1/3060-moq-net-ban-hop-id-0-from-hop-chains.md) - moq-net: ban Hop ID 0 from hop chains
- [#1059](/quest/m1/1059-init-tracks-for-cmaf.md) - Init tracks for CMAF
- [#2248](/quest/m1/2248-moq-mux-rebase-fmp4-export-timestamps-for-late-subscribers.md) - moq-mux: rebase fMP4 export timestamps for late subscribers
