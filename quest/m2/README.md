# m2: later work

## Goal

Independently shippable features, internal optimizations, and broad tooling
follow the clustering and API release. Publication and downstream adoption
also live here because they necessarily follow m1; they are not abandoned.

## Plan

Decided 2026-10-13: a feature does not block the breaking release merely
because it was in the next-wave backlog. Keep existing behavior correct in
m1, and defer new capabilities without changing their intended outcome.
Dependencies on m1 remain explicit. An implementation that discovers a new
necessary API break brings that decision to the maintainer before proceeding.

Decided 2026-11-03 (maintainer, Kixel's quest audit): [byte streams](/quest/m2/stream/README.md)
and the [mesh libraries](/quest/m2/mesh/README.md) rank first in m2, ahead
of [release execution](/quest/m2/release/README.md). Kixel's daemon builds on
them from `main` through a git submodule, so it needs them merged, not
released. They stay out of m1 so the cut does not wait on them; each starts
as soon as its m1 prerequisites (sockets, fastest-reader pacing, outbound
federation) land.

Rate-grant work targets the next unpublished lite version after finalized
lite-07; bitrate enforcement is not part of this release.

## Required

- [Byte streams](/quest/m2/stream/README.md) - TCP connections ride MoQ losslessly through relays, proxied per hop rather than as IP packets
- [Mesh libraries](/quest/m2/mesh/README.md) - an `Endpoint` reaches any enrolled node by endpoint ID over relays or a sparse direct overlay, with address hints and a `.status` track; the mesh binary and UI live in Kixel
- [`@moq/net` broadcasts expose their requests](/quest/m2/net-broadcast-requests.md) - a JS broadcast serves tracks it never created, on demand, like Rust
- [`@moq/crypto` loads a stored signing key](/quest/m2/crypto-key-load.md) - one device key proves mesh identity and signs broadcasts
- [Announce credit](/quest/m2/announce-credit.md) - a subscriber bounds an Announce Stream's routes with credit at the wire, so a path flood never closes a session
- [JS announced coalescing](/quest/m2/js-announced-coalesce.md) - `@moq/net`'s `Announced` keeps one pending event per path, like Rust
- [An audio track's final flush joins its open group](/quest/m2/audio-flush-group.md) - a zero-`delay.max` subscriber gets the tail, not `Old`
- [Release execution](/quest/m2/release/README.md) - publication, installed-artifact proof, cleanup, and downstream adoption after m1
- [One policy for oversized container metadata](/quest/m2/import-metadata-policy.md) - fMP4 and MKV refuse or stream huge metadata by one documented rule
- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - the cmsg walks in moq-sock and moq-uring have invariant fuzz targets, and the unsafe peer input reaches runs under Miri
- [VA-API renderer tests](/quest/m2/vaapi-render-tests.md) - the VA-API renderer, decode, and transcode tests run in `just rs gpu` on AMD and Intel and fail instead of skipping, and hosted CI reports them ignored
- [One Ruff pin and Swift formatting on aarch64-linux](/quest/m2/format-gate-followups.md) - only the dev shell pins Ruff, and aarch64-linux checks Swift formatting once nixpkgs builds `swiftc` there
- [Weekly mutation testing on critical modules](/quest/m2/mutants.md) - cargo-mutants runs weekly on moq-auth and moq-net's permission, resume, and expiry code
- [Video quality](/quest/m2/video-quality/README.md) - a nightly VMAF harness grades every encoder backend and on-demand GoP pops against floors, and moq-boy's encode is fixed under it
- [P2P](/quest/m2/p2p/README.md) - application-selected peer sessions use shared priorities, routing, and same-epoch handover
- [moq-auth drops the unused peer grant API](/quest/m2/auth-drop-peer-grant.md) - P2P trusts its peers, so `PeerGrant` and its `KeySet` methods go
- [rust2's exporter is ready for moq](/quest/m2/rust2-exporter.md) - waiting on moq-dev/rust2's exporter to generate every language moq ships
- [Switch to rust2's exporter](/quest/m2/ffi-exporter.md) - moq's bindings come from rust2-ffi, moq-ffi shrinks to its remainder, and moq's UniFFI generator code is deleted
- [Generated C bindings](/quest/m2/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written crate
- [A C origin can discover eagerly, like a relay](/quest/m2/c-origin-relay.md) - the C API puts an origin in relay mode, so forwarding a client session's routes needs no local announce watcher
- [A runtime-free QUIC driver](/quest/m2/quic-sansio-driver.md) - a caller's own loop moves UDP datagrams and polls moq sessions over moq-quic, for rust2's bindings
- [Benchmark sources](/quest/m2/bench-sources.md) - every measuring nightly benchmark feeds the regression table, and none is a smoke run
- [QoS](/quest/m2/qos/README.md) - the relay's egress `lag` histogram and `dropped` counters on the stats totals and prefix tracks
- [HLS and DASH browser playback](/quest/m2/hls-player-test.md) - hls.js and dash.js play, reload, switch, and survive reconfiguration nightly
- [The DASH manifest keeps segments across a reconfigure](/quest/m2/dash-reconfigure.md) - a same-codec reconfigure keeps the old instance's segments listed in DASH, as HLS does
- [Broadcast fairness](/quest/m2/broadcast-fairness.md) - broadcasts share a cluster link fairly, so one tenant's priority never starves another's
- [Cache fair share](/quest/m2/cache-fairness.md) - one tenant cannot crowd another out of the relay's shared group cache
- [Peer grant bound](/quest/m2/peer-grant-bound.md) - a peer's AUTH_OK costs near-linear time in its bytes on every input shape, in Rust and in a browser
- [A regrant re-checks only the routes it can change](/quest/m2/regrant-route-recheck.md) - an `AUTH_OK` no longer costs routes times grant members on every lite announce stream
- [Plan: watch worker](/quest/m2/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m2/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - H.264 and H.265 from an external Vulkan image on RADV, hand-rolled on ash, proven on an RX 9070
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - an explicit-sync DMA-BUF reaches Intel's encoder, proven on Arrow Lake
- [C++ FFI reuse](/quest/m2/cpp-ffi-reuse.md) - C++ packages link the shared default-feature FFI build once it is proven compatible
- [Upstream reports](/quest/m2/upstream-reports.md) - report the faad, retina, and mr-boxington bugs found before the cut

- [Native compression flush](/quest/m2/native-compression-flush.md) - report the per-frame nonfinal flush requirement in existing browser discussions
- [Native synchronous codecs](/quest/m2/native-compression-sync.md) - report the stateful small-frame synchronous API requirement
- [Native browser codecs](/quest/m2/native-compression-adopt.md) - adopt native operations when timing, frame and resource contracts hold

- [moq-net's library tests pass under cargo test](/quest/m2/net-test-subscriber.md) - two moq-net tests stop each installing a global tracing subscriber, so plain `cargo test -p moq-net --lib` passes

- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - replace FFmpeg video and audio decoding with moq-video and moq-audio, deliver GPU frames, and use native audio/video encoders
- [MoQ in obs-studio](/quest/m2/obs-studio/README.md) - a native MoQ output and service merged into obs-studio, once its maintainers agree; the plugin carries it until then
- [AudioToolbox decode](/quest/m2/audio-decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [AudioToolbox encode](/quest/m2/audio-encode-audiotoolbox.md) - macOS and iOS encode AAC-LC
- [GPU CI](/quest/m2/gpu-ci.md) - NVIDIA tests gate the merge queue on a self-hosted GPU runner VM, and `just rs nvidia` runs them locally instead of skipping
- [Vendor-free CARLA bridge](/quest/m2/carla-bridge.md) - the CARLA demo's GPU bridge lets moq-video pick the encoder, and its stand-in test runs on AMD as well as NVIDIA
- [Probe](/quest/m2/probe/README.md) - a session probes up to the bitrate it wants to send or receive, so a stopped simulcast rendition resumes and a viewer up-switches; the browser pads where native sessions retransmit
- [Media stats](/quest/m2/stats/README.md) - publishers announce a stats track in the catalog, viewers answer a soliciting catalog through a per-catalog `.echo` broadcast, and one model turns both into a health verdict and a preflight report
- [Perf](/quest/m2/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [Wire stats](/quest/m2/wire-stats/README.md) - `Presence` counts each root's transport bytes on the wire, `wire_tx` and `wire_rx`, from QUIC, WebSocket, and embedder gateway sources
- [Performance survey follow-ups](/quest/m2/performance-survey/README.md) - benchmark the source audit's transport, browser, media and serving risks before optimizing
- [Transport x version benches](/quest/m2/bench-transport-sweep.md) - the session benches sweep transports as well as versions
- [Shared group serving and ingest](/quest/m2/shared-serve.md) - Rust's moq-lite and moq-transport sessions serve and ingest groups through one poll machine each, with only wire encoding per protocol
- [Shared group serving and ingest in JS](/quest/m2/shared-serve-js.md) - the same split in js/net, following the Rust seam
- [Paired bench BASE](/quest/m2/bench-base-paired.md) - `just bench <base>` uses the nightly's paired comparison and the old one is deleted
- [Receive pool](/quest/m2/receive-pool.md) - one byte budget per worker covers every connection's receive credit and the cache, billing each frame once
- [Front parking](/quest/m2/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [Route wakes](/quest/m2/route-wakes.md) - a route change wakes only the fronts it can move, so pool churn stops scaling with served paths
- [A route stream's interest is a set of prefixes](/quest/m2/route-interest-prefixes.md) - one Announce Stream covers several subtrees, with floors and certification proved over the set
- [Fewer allocations in announce fan-out](/quest/m2/announce-fanout-allocs.md) - a deep path costs no more allocations per subscriber than a shallow one
- [moq-room scales to large rooms](/quest/m2/room-bench.md) - a benchmark measures `Room` with large participant counts, and polling narrows only if it shows a slope
- [Route health](/quest/m2/route-health/README.md) - epoch-only active/passive track recovery over metadata-only subscriptions
- [Wrapper traits over moq-net](/quest/m2/net-wrap/README.md) - moq-net exports producer and consumer traits, and every writer and reader above it takes them, so a wrapper such as e2ee passes through unchanged
- [E2EE](/quest/m2/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Content protection](/quest/m2/drm/README.md) - Common Encryption CMAF publishes unchanged, signalled in the hang `drm` section and MSF `contentProtections`, with license URLs from Rust, the CLI, and every binding
- [iOS capture](/quest/m2/mobile-capture-ios.md) - Rust captures the camera and screen on iOS, setting the catalog rotation
- [Android capture](/quest/m2/mobile-capture-android.md) - Rust captures through NDK/JNI on Android, reusing the existing codecs and setting the catalog rotation
- [Latest readers use Range::live()](/quest/m2/readers-range-live.md) - after #6278, readers start at the live edge without waiting on `largest()`
- [Audio outlives video](/quest/m2/audio-bufferbloat-av.md) - under bufferbloat audio stays within budget while video may skip, rendered frames stay in sync, and native audio rows are enforced
- [ViSQOL grading](/quest/m2/audio-visqol.md) - the nightly scores played audio against a reference clip and fails below measured floors
- [PulseAudio capture periods](/quest/m2/pulseaudio-capture-period.md) - capture on cpal's PulseAudio host arrives in short periods, not the server's 2 s default fragment, proven on the null sink's monitor
- [Bindings size profile](/quest/m2/ffi-size-profile.md) - a benchmark decides whether the moq-ffi builds ship at opt-level "s", which halves the dylib
- [Value merge-patch diff](/quest/m2/json-value-diff.md) - benchmarks decide between moq-json's generic serializer diff, a ported `Value` diff, and compression alone
- [Measure what a catalog republish costs per rendition edit](/quest/m2/catalog-republish-cost.md) - bench whole-catalog re-serialization across track counts, then decide deltas or a bound
- [A JSON snapshot consumer resyncs after a bad frame](/quest/m2/json-snapshot-resync.md) - a decode failure drops the group with a typed error, so `@moq/room` and moq-boy resume at the next snapshot
- [Go module proxy](/quest/m2/go-module-proxy.md) - `moq.dev/moq-ffi` ships from a static GOPROXY we host, so its staticlibs stop growing git history by ~61 MiB per release
- [Go per-platform lib modules](/quest/m2/go-ffi-platform-modules.md) - a Go build downloads only the host's staticlib, and `go mod vendor` works
- [Dart on iOS](/quest/m2/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [Mobile completion](/quest/m2/mobile-completion.md) - verify the Swift and Kotlin bindings over Rust-owned capture
- [Browser publishing paths under test](/quest/m2/publish-firefox.md) - the media harness publishes from Firefox in Nightly and passes, covering the `<video>` capture fallback Chromium never takes
- [Bindings in the interop matrix](/quest/m2/interop-bindings.md) - Kotlin, Swift, and Dart publish and subscribe against every other interop client
- [WebGPU HDR](/quest/m2/webgpu-hdr.md) - HDR renditions play as HDR where the browser and display can show it, and tone-map to SDR elsewhere
- [Resume as producer replacement](/quest/m2/resume-redesign.md) - route changes replace the writer of one track state, deleting the per-reader resume path in Rust and JS
- [Migrated sessions close once their replacement synced](/quest/m2/goaway-migration-followups.md) - moq-tokio holds the early close for the replacement's announce sync, and the JS `Connection` closes early too
- [moq-transport requests end through one lease](/quest/m2/ietf-request-lease.md) - every IETF request ends through one polled lease, and the two 18k-line files split by mechanism
- [A moq-transport group head fills its hole in place](/quest/m2/ietf-fill-refill.md) - fills and joining FETCHes write into the tail's group, deleting the `Fill` handoff
- [moq-transport hole markers](/quest/m2/ietf-hole-marker.md) - a negotiated extension names a SUBSCRIBE's holes, so our publishers answer multi-group FETCH from caches that know every group; built when an IETF client wants it
- [Announce interest streams share one route table](/quest/m2/announce-interest-memory.md) - per-stream announce cost follows what is in flight, not every route
- [One relay serve path for every runtime](/quest/m2/relay-serve-path.md) - io_uring serves through the shared `Connection::serve` and config mapping
- [A parked read of a resumed group reads its budget without re-syncing](/quest/m2/recover-budget.md) - measure, then drop the per-frame reader sync
- [One port](/quest/m2/one-port/README.md) - one UDP port carries QUIC, SRT, and WebRTC media through an embedder hook, and one TCP port carries HTTP, RTMP, and RTMPS
- [WebSocket caps](/quest/m2/rate-websocket.md) - paced reads and writes over bounded socket buffers hold WebSocket to the same caps
- [Publishers learn their cap](/quest/m2/rate-grant.md) - the AUTH grant carries the caps and publishers clamp their encoder to them
- [moq-rtc pacing](/quest/m2/rtc-pacing.md) - a capped WHEP viewer never receives more than its cap plus burst, NACK resends included
- [SRT rate handle](/quest/m2/srt-rate-handle.md) - a per-connection cap paces SRT pulls, retransmits included, and closes SRT publishers past it
- [One port on the io_uring workers](/quest/m2/uring-demux.md) - `moq-uring`'s workers host the UDP demux, so a ring relay keeps WebRTC media and SRT on its QUIC port
- [Synced data playback](/quest/m2/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [Watch decode gate](/quest/m2/watch-decode-gate.md) - video lookahead stays encoded until it is near presentation, like audio
- [moqsrc A/V sync](/quest/m2/moqsrc-av-sync.md) - a nightly test measures A/V skew from `moqsrc` through clock-synced sinks
- [fMP4 ambisonic layout](/quest/m2/mux-vr-ambisonic-fmp4.md) - fMP4 Opus import fills `ambisonic` from the `dOps` mapping table
- [Linux decoded frames](/quest/m2/obs-decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Windows decoded frames](/quest/m2/obs-decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [macOS GPU input](/quest/m2/obs-macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m2/obs-windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md) - try the Rust implementation as JS-only browser output and compare source-translator alternatives if needed
- [JS-only @moq/net](/quest/m2/rs2ts-js.md) - adopt the selected Rust-to-JS route after browser parity and measured tradeoffs are accepted
- [Per-stream and per-datagram deadlines](/quest/m2/quic-deadline.md) - hopeless retransmits
  become resets, a tail loss probe fires early while there is still time, and
  datagrams past their subscription's max delay are dropped instead of sent late
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - the
  backend-neutral hook on moq-net's own transport trait that awaits an
  acknowledged stream offset, implemented by the moq-tokio and moq-uring adapters
- [Delivery delay](/quest/m2/delivery-delay.md) - a per-frame
  delivery-delay histogram from `poll_acked`, so viewer jitter is visible
- [#3199](/quest/m2/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup, if a profile shows the win
- [#3200](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [Browser media report](/quest/m2/rs2ts-browser-report.md) - compare Rust-backed and handwritten @moq/net through full media workloads, supplementing the pre-adoption comparisons
- [Link quality](/quest/m2/link-quality.md) - a radio link's cost follows its measured quality without flapping routes
- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - Linux relay packages ship io_uring once the ring is on par with tokio
- [io_uring thread check](/quest/m2/uring-transport-machines.md) - the relay fanout bench measures `moq_uring::transport::Local`'s per-call thread check after the transport is boxed, removing it only if it shows
- [io_uring quiescence on a quiet host](/quest/m2/uring-bench-followups.md) - quiet-host acceptance numbers and a nightly `worker_turns` QUIC request-reply series
- [Opus implementation](/quest/m2/audio-opus-backend.md) - compare Opus codec quality, CPU, build cost, and the loss recovery each backend offers
- [Latency ledger](/quest/m2/latency-ledger.md) - a viewer reports its share of playback delay stage by stage: jitter buffer and device
- [Media Foundation decode](/quest/m2/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [NVENC buffer pool](/quest/m2/nvenc-pool.md) - NVENC reuses input and output buffers instead of allocating per frame, if a benchmark shows it wins
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Media Foundation encodes on the texture's adapter](/quest/m2/mf-encode-adapter.md) - Windows machines with two GPUs keep an exported texture on the GPU route into a hardware encoder
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - video with periodic intra refresh imports and plays back cleanly with one group per sweep and a catalog `warmup`
- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer on hardware
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into caller-owned buffers with no copy, once a profile says where
- [BBR media study](/quest/m2/quic-bbr-study.md) - whether Google's growth recognition and precautionary probe stop help media, measured on a shaped media harness
- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - native publishers pick `Realtime`, a receive-ts delay-based controller that keeps the uplink queue near empty for teleoperation
- [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) - the WGC display and window backend verified on real Windows hardware
- [Windows capture parity](/quest/m2/capture-windows.md) - system and per-app audio and a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture, per-app audio, and explicit display-selection and app-capture limits
- [Per-app audio on macOS](/quest/m2/capture-app-audio-macos.md) - one application's audio through ScreenCaptureKit
- [cpal loads libasound at runtime](/quest/m2/cpal-alsa-runtime.md) - condition: a cpal release whose Linux build carries no load-time libasound requirement
- [Audio capture without ALSA link](/quest/m2/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound, and capture becomes a default feature
- [The moq CLI can publish a changing catalog](/quest/m2/cli-catalog-updates.md) - catalog updates mid-stream, so recovery replays need no hand-built clients
- [Ship capture and playback](/quest/m2/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [Egress profile](/quest/m2/quic-egress-profile.md) - measure relay send-path syscalls, pacing bursts, and allocations before optimizing any of them
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - H.265 encode and decode, pre-generated bindings, and pooled resize surfaces, including the moq-dev/vaapi release that carries them
- [GPU capacity and health](/quest/m2/gpu-health.md) - moq-video reports each device's sessions, memory, utilization, and health on NVIDIA, AMD, and Intel alike
- [A release carries multi-vendor GPU input](/quest/m2/gpu-release.md) - `release` carries the GPU surface validated on AMD and Intel; health follows separately
- [The remaining library spawns](/quest/m2/spawn-audit.md) - every async spawn left in a library crate becomes a polled `kio::Tasks` child or is listed with why it stays
- [The relay over QUIC on the sim](/quest/m2/quic-sim.md) - the relay and moq-tokio's `Client` and `Server` run over `moq-quic` on the seeded sim
- [moq-shaper's own socket never passes for path loss](/quest/m2/shaper-socket.md) - relay-sized buffers and its own socket drops reported beside path drops

- [DVR rewind](/quest/m2/archive-dvr.md) - follow-on capability after the release contract is stable
- [Archive lookups cancel when abandoned](/quest/m2/archive-lookup-withdrawal.md) - a public withdrawal signal on `Fill` stops a time lookup's store read once its readers leave

- [Demo dashboard shows viewer lag](/quest/m2/qos-lag-dashboard.md) - follow-on capability after the release contract is stable

- [Publisher timeliness at relay ingest](/quest/m2/qos-publisher-timeliness.md) - follow-on capability after the release contract is stable

- [Auth session caps cost O(1)](/quest/m2/auth-session-index.md) - per-token and per-remote counts and a time-ordered expiry queue replace the scans, after #7211
- [The relay refuses a cluster peer it cannot dial](/quest/m2/relay-undialable-peers.md) - moq-tokio reports the schemes it can dial, and the relay refuses an undialable peer at startup instead of retrying forever
- [Drop moq-auth's iroh transport](/quest/m2/auth-drop-iroh.md) - the next breaking moq-auth release drops `Transport::Iroh` and the JS `"iroh"` value
- [moq-transport finish interop](/quest/m2/ietf-finish.md) - the interop `finish` case runs on every even moq-transport draft instead of being skipped
- [JS joining FETCH](/quest/m2/ietf-js-joining-fetch.md) - the JS publisher answers a joining FETCH that arrives after a quick finish, mirroring the Rust bound
- [An Absolute join never races an older-history FETCH](/quest/m2/ietf-absolute-join-race.md) - reproduce #7680's suspected collision on main, then fix it or keep the test and delete
- [Relays and moq-tokio offer versioned qmux-02 first](/quest/m2/qmux-02-subprotocols.md) - one shared qmux version list prefers 02, so JS clients negotiate it
- [A passing interop run stops publishers cleanly](/quest/m2/interop-shutdown-warnings.md) - no lingering publisher or unfinished-producer warning in a green interop run
- [Smoke tests make their certificates without a system OpenSSL config](/quest/m2/smoke-libressl.md) - moxygen and Cloudflare smoke pass `-config`, as the conformance runner does
- [ashpd uses maintained D-Bus crates](/quest/m2/ashpd-maintained.md) - wait for a stable zbus 6 port, then remove the portal dependency exceptions
- [Askama uses a maintained TOML parser](/quest/m2/askama-maintained.md) - wait for a stable parser migration, then remove the generator dependency exception
- [Dependency and alternatives audit](/quest/m2/audit-deps.md) - every dependency earns its weight, preferring fewer, and the rest are current, deduplicated, and the best maintained option
- [Dead code and API surface audit](/quest/m2/audit-dead-code.md) - repeat the removal audit before the next major release
- [Typecheck with bun check](/quest/m2/bun-check.md) - workspace packages typecheck through bun once nixpkgs ships bun 1.4.4
- [Adopt formatter defaults after evaluating Bun](/quest/m2/formatter-defaults.md) - every language uses its formatter's defaults, YAML and Markdown are formatted, and remark is gone in one coordinated migration
- [Test coverage audit](/quest/m2/audit-test-coverage.md) - repeat the coverage audit against the m1 baselines after the next release ships
- [Documentation audit](/quest/m2/audit-docs.md) - repeat the docs audit after the release ships, against the m1 baselines
- [Test cost audit](/quest/m2/audit-test-cost.md) - the slowest, flakiest, and least valuable tests are deleted by default, or else fixed or planned, once the release ships

- [Quest tree audit](/quest/m2/audit-quests.md) - repeat the whole-tree audit after the release ships, against the m1 baseline
- [CI workflow audit](/quest/m2/audit-ci.md) - repeat the per-workflow and per-job review after the release ships, against the m1 baselines
- [Performance audit](/quest/m2/audit-perf.md) - benchmarks that regressed since the m1 run are explained, fixed, or planned, and unbenched hot paths get coverage
- [Simplification audit](/quest/m2/audit-simplify.md) - repeat the simplification audit, starting with the m1 leftovers, after the release ships
- [Upstream unknown-env option in usage-rs](/quest/m2/usage-unknown-env.md) - jdx/usage refuses unknown prefixed env vars, replacing the moq-tokio helper
- [Upstream single-declaration settings in usage-rs](/quest/m2/usage-settings-once.md) - waiting on approval to post, then jdx/usage builds the registry from Cli fields
- [Artifact size audit](/quest/m2/audit-size.md) - repeat the shipping, native, and browser attribution after the release ships
- [Compile time audit](/quest/m2/audit-compile-time.md) - build-time hotspots are measured against the m1 baselines, starting with the transport share now that moq-net compiles once, and the worst are removed, made cheaper, or planned
- [Lint override and deprecation audit](/quest/m2/audit-lints.md) - repeat the suppression and deprecation audit after the release ships, starting with clippy on the macOS and Windows targets
- [Archive reads against a real store](/quest/m2/archive-store-bench.md) - LIST/GET counts and latency over track count
- [Branch checks lint examples](/quest/m2/lint-examples.md) - `just check` runs clippy on the selected crates' examples, and PR CI builds the C++, Go, and Python interop clients
- [Drop kio's epoch fence](/quest/m2/crossbeam-pin-fence.md) - waiting on crossbeam#1352, then the `Read::drop` fence goes
- [A cancelled miss stops waiting](/quest/m2/fill-wait-cancel-test.md) - a test proves a relay miss waiting on its own fill ends when no reader wants it
- [No MOQ_* knobs outside the binaries](/quest/m2/env-knobs.md) - test, bench, and library knobs leave the `MOQ_*` prefix
- [Clean ALSA playback onset](/quest/m2/audio-alsa-onset.md) - no onset restarts or clicks over PulseAudio
- [Interop audit](/quest/m2/audit-interop.md) - our interop runner entries match what we ship, and every red interop cell with a moq.dev side is fixed, reported, or noted as down or unsupported
- [moq-hls exposes each rendition's bound source](/quest/m2/hls-rendition-source.md) - playlists and segments report the source path and instance they read, so a cache can key on the source
- [The HLS server follows each path once](/quest/m2/hls-follow-once.md) - `Following` reports synced absence, so moq-hls resolves with one follow
- [moq-bench pins the instances it measures](/quest/m2/bench-epoch-pin.md) - pooled requests pin their announced epoch, so a takeover is not counted as the same broadcast
- [moq-bench reports sub-millisecond latency](/quest/m2/bench-latency-resolution.md) - steady-window latency resolves finer than 1 ms
- [Benchmark series stay meaningful](/quest/m2/bench-hygiene.md) - a warm `burst` variant, stale series pruned, and slow-group `bytes` kept out of series names
- [The memory bench finishes at 1 MiB frames](/quest/m2/memory-bench-hang.md) - the allocation mode's park at `size=1048576` is root-caused and fixed
- [Audio's Latest start reaches back](/quest/m2/audio-latest-reach-back.md) - moq_audio's start covers min plus warmup like other consumers
- [moq-lite tails count drops](/quest/m2/lite-tail-drop-count.md) - SUBSCRIBE_SKIP and SUBSCRIBE_DROP accounting replaces the session-tail grace from lite-05
- [Re-run the SPS fuzzer on h264-reader](/quest/m2/sps-fuzz-h264-reader.md) - the H.264 SPS overflow is rechecked on the new parser
- [Drop the H.265 SPS pre-walk once scuffle is fixed](/quest/m2/scuffle-sps-bump.md) - waiting on ScuffleCloud/scuffle#653 and #654 releases, then bump and drop the walk
- [Audio track changes are gapless](/quest/m2/gapless-audio-switch.md) - no silence beyond a frame on audio format or rendition changes, enforced by media QA
- [PCM encodes in one pass](/quest/m2/pcm-bulk-encode.md) - the PCM encoder converts a frame in bulk instead of one sample at a time
- [We own our WebRTC stack](/quest/m2/webrtc/README.md) - moq-rtc becomes moq-webrtc with moq-ice, the data-channel transport, and a vendored, trimmed str0m/dimpl/is/sctp-proto
- [PUBLISH_DONE says why](/quest/m2/publish-done-codes.md) - GOING_AWAY, TOO_FAR_BEHIND, and EXPIRED round-trip as stream codes
- [A revoked announce resets instead of FINning](/quest/m2/announce-revoke-reset.md) - a PUBLISH_NAMESPACE revoked after its OK resets with INTERNAL_ERROR, per draft-22 §6.4.2.3
- [moq repair refuses a live recording](/quest/m2/mp4-repair-lock.md) - the recorder's advisory lock keeps `moq repair mp4` off a file still being written
- [The error-queue reader decodes truncated control data](/quest/m2/errqueue-truncation.md) - `MSG_ERRQUEUE` keeps what fit on `MSG_CTRUNC`, like the receive paths
- [The QUIC driver tells an MTU probe from an oversized send](/quest/m2/quic-emsgsize-classify.md) - a non-probe `EMSGSIZE` is logged and counted instead of dropped silently
- [moq-quic's internal dev-dependencies are path-only](/quest/m2/quic-dev-dep-path.md) - moq-congestion is a path dev-dependency
- [moq-mux's AAC config reports SBR](/quest/m2/aac-sbr-config.md) - moq-audio reads backward-compatible SBR from the config
- [fMP4 decode times from the declared reorder depth](/quest/m2/fmp4-declared-clock.md) - `DeclaredClock` replaces the sub-GOP cadence cap, as in TS, FLV, and MKV
- [An ended HLS playlist has no trailing gap](/quest/m2/hls-trailing-gap.md) - no zero-duration `EXT-X-GAP` before `EXT-X-ENDLIST`
- [moq-video's Apple Sink encodes on its own thread](/quest/m2/apple-sink-thread.md) - macOS and iOS ladders never encode on a tokio worker
- [/fetch answers from the announce sync](/quest/m2/fetch-synced.md) - a missing broadcast gets a 404 once announcements are synced, not after 30 s
- [An advertisement stuck on a zero-credit peer gives up](/quest/m2/advertise-credit-timeout.md) - A Rust moq-transport publisher whose PUBLISH_NAMESPACE or namespace update cannot leave because the peer grants no send credit gives up on it within a bounded time, as it already does on a silent answer: it resets the request and frees its state
- [aec::Processor's edge cases are tested](/quest/m2/aec-tests.md) - moq-audio tests cover `aec::Processor` at the extremes of its accepted input
- [A failed archive time lookup fails the subscription](/quest/m2/archive-time-followups.md) - when an `archive::Consumer` cannot answer a `Time(pts)` lookup because a store GET fails or an object does not decode, the subscription fails with a typed error naming the lookup failure instead of silently falling back to the cache
- [moq-sock's control buffer is sized for the send options too](/quest/m2/cmsg-len-send.md) - `cmsg::LEN` in moq-sock is derived from, or checked at compile time against, both the receive options and the send options
- [A panic inside moq-tokio's connection internals fails callers instead of panicking them](/quest/m2/connection-panic-isolation.md) - after a moq-tokio QUIC connection driver panics, every client-side caller gets the connection's terminal error instead of panicking in its own task, and the certificate reload watcher and qmux driver tasks are supervised
- [No large future in the FFI layers can overflow a runtime or foreign thread's stack](/quest/m2/ffi-future-size.md) - every future moq-c and moq-ffi spawn or block on is below a size bound that a lint enforces, so unoptimized builds stay inside the runtime thread stack
- [A browser test covers GPU encoder death and a broadcast rename](/quest/m2/gpu-encoder-death-test.md) - A real-browser test makes `@moq/publish`'s video encoder fail mid-stream
- [An empty tail stream for a headless moq-transport group creates no group](/quest/m2/headless-empty-tail.md) - when a moq-transport subscription's fill for the Largest Group fails, an empty subgroup stream for that headless group is dropped instead of creating an empty group that reads as complete
- [A recording stalled on an open group reports what it waits for](/quest/m2/hls-stalled-segment.md) - when a moq-hls recording waits on a listed segment whose group a still connected publisher never closes, the wait is visible
- [A kept draft-18 join pins its head group](/quest/m2/ietf-join-pin-head.md) - on draft 18, a joining FETCH that our publisher keeps open after a quick finish is answered even if the publisher drops its `track::Producer` right after `finish()`
- [The JS player keeps a track's start when decoding from its first packet](/quest/m2/js-warmup-trim.md) - `@moq/watch` trims an audio track's catalog `warmup` only on a mid-stream join, as moq-audio does
- [kio::Tasks tolerates a removed task's waker firing into its reused slot](/quest/m2/kio-tasks-waker-test.md) - A unit test in `rs/kio/src/task.rs` shows that when a task taken out with `Tasks::remove` wakes after another task was `insert`ed into its slot, the new occupant is polled at most once spuriously, still finishes on its own wake (handed back through `poll_each`), and its `TaskKey` still removes it
- [moqsrc reconnects correctly whatever order events arrive in](/quest/m2/moqsrc-reconnect-order.md) - A moq-gst test reconnects `moqsrc` while a rendition pad is blocked downstream, delivering the reconnect's events
- [A library recorder gets the takeover cut](/quest/m2/mux-source-cut.md) - A recorder built directly with `moq_hls::export::Broadcaster::new` ends a replaced instance's recording at what it listed when another publisher takes the broadcast over, exactly as one built through `Server::broadcaster` does
- [The relay NixOS module's peer list is tested](/quest/m2/nix-relay-peers-test.md) - `nix flake check` evaluates the moq-relay NixOS module with no, one, and several `cluster.rootUrl` peers and refuses an empty list, asserting the `MOQ_CLUSTER_CONNECT` value each produces
- [The OBS source joins audio at the live edge](/quest/m2/obs-decode-latest.md) - the OBS MoQ source's audio decoder starts at the newest cached group instead of replaying the track's backlog, so a reconnect or rendition change plays live audio at once
- [OBS simulcast layers reach moq-watch from a live OBS, including the dock's Go Live](/quest/m2/obs-multitrack-check.md) - A live OBS 32.2.2 run, recorded in the PR, shows the `WHIPSimulcastTotalLayers` profile setting attaching two or more scaled video encoders to `moq_output`, each published as its own rendition and watched in `<moq-watch>`, both through the obs-websocket recipe in `doc/bin/obs.md` and through the MoQ dock's Go Live, which honors the profile's layer count instead of creating a single encoder
- [Opus export never writes a channel mapping it can't stand behind](/quest/m2/opus-surround-export.md) - exporting a catalog Opus rendition with 3 to 8 channels and no description writes a `dOps` with the Vorbis family 1 mapping table, as the TS importer assumes, instead of an invalid family 0 `dOps`
- [Open-GOP MPEG-2 export trims leading B-pictures in zero-stuffed CBR streams](/quest/m2/pes-hold-open-gop.md) - A TS export starting or resuming inside an open GOP of MPEG-2 video drops the orphaned leading B-pictures even when their PES ends on zero stuffing, as CBR encoders emit, without ever trimming part of a reference picture whose start code straddles two PES packets
- [The lite publisher tells transport faults from cancels](/quest/m2/publish-cancel-classify.md) - `rs/moq-net`'s lite publisher no longer counts every `Error::Transport` as a clean cancel
- [moq-publish hides previews that show nothing](/quest/m2/publish-empty-previews.md) - `<moq-publish>` hides a preview that renders nothing
- [moq-quic's sim pair fails a test that sends an oversized packet](/quest/m2/quic-sim-mtu.md) - A moq-quic test whose endpoint sends a datagram larger than the simulated path MTU fails, instead of the `Pair` silently dropping it
- [A stalled reader within budget gets no Old resets through a relay](/quest/m2/relay-capped-credit-test.md) - A test proves that a subscriber which stalls before its first read, behind a relay over a transport with capped stream credit, receives no `Old` stream resets for groups still within its max-delay budget, and reads them in order once it resumes
- [In-memory transports refuse a request without stream credit](/quest/m2/sim-credit-open.md) - moq-net's in-memory test transports model the peer's bidirectional stream credit, and `poll_open_request` returns `Ok(None)`
- [The tenant matrix's HTTP checks assert exact refusals](/quest/m2/tenant-leak-checks.md) - the relay tenant matrix's HTTP check asserts a definite outcome for each cross-tenant request, so it cannot pass by timing out or by an unexpected status
- [The test inventory follows every branch selection](/quest/m2/test-inventory-followups.md) - `sh/gh/test-inventory.py` judges every check by the dispatcher branch its caller actually selects, including the cargo-fuzz caller check, the recipe-runs-tests check
- [tls:// sessions report their negotiated ALPN](/quest/m2/tls-link-alpn.md) - A session accepted on a `tls://` listener reports the negotiated moq ALPN in `Link::alpn`, as raw QUIC, WebTransport, and WebSocket sessions already do, so admission and logs see the protocol on every transport
- [PUBLISH token renewal is consistent across pause and forward, and the multi-token cost is profiled](/quest/m2/token-auth-followups.md) - on a moq-transport session, a REQUEST_UPDATE carrying an AUTHORIZATION TOKEN on a PUBLISH gets the same answer whether the PUBLISH is paused or forwarding
- [HTTP auth servers see which request named a token](/quest/m2/auth-http-request-kind.md) - the HTTP auth contract forwards `auth::Request`'s `kind()` and `path()`
- [moq-uring's handle lifecycle tests cover the newly spawned queue and a woken close watcher](/quest/m2/uring-lifecycle-tests.md) - two regressions cover the shutdown paths moq-dev/moq#7309 added
- [Watch tests wait for the change they expect](/quest/m2/watch-test-waits.md) - `js/watch` tests wait for the signal or state they assert on instead of a fixed number of event-loop yields, so they neither flake under load nor pass before the change happens
- [JS eviction tests prove the eviction happened](/quest/m2/track-test-eviction-asserts.md) - `track.test.ts`'s eviction tests fail with eviction off
- [Nightly runs zizmor's online workflow audits](/quest/m2/workflow-audit-followups.md) - nightly runs zizmor's online audits
- [moq auth sign mints capped tokens](/quest/m2/auth-sign-caps.md) - CLI flags for the bitrate caps
- [QUIC-LB forwarding reads the cluster's peers and reports its counts](/quest/m2/quic-lb-forward-extras.md) - a cluster-API peer map and forward counters
- [An audio-only broadcast that can't play hides its play button](/quest/m2/audio-only-play.md) - center-play follows the audio-only rule, proven by a browser test
- [moq-quic's long header type is one variant](/quest/m2/quic-long-type.md) - collapse LongType after the 0-RTT removal
- [One predicate for the lite close wait](/quest/m2/subscriber-fin-fold.md) - fold waits_for_subscriber_fin into has_track_stream
- [Browsers expose WebTransport receive credit](/quest/m2/browser-receive-credit.md) - condition: the page can tell how much credit an unread stream holds
- [JS fastest-reader pacing](/quest/m2/js-fastest-reader.md) - `@moq/net` paces upstream groups by the fastest reader, once browsers allow it
- [fMP4 import at 1-byte chunks is no slower than before](/quest/m2/fmp4-byte-chunk-perf.md) - bisect and fix or accept the ~19% slowdown at 1-byte chunks
- [The relay bench reports io_uring pool size and RSS](/quest/m2/bench-uring-pool.md) - pool growth regressions show up in normal bench runs
- [MKV imports ffmpeg's multichannel FLAC](/quest/m2/mkv-flac-channel-mask.md) - a channel-mask-only Vorbis comment is accepted, so default ffmpeg multichannel FLAC needs no remux
- [The bindings' TrackInfo docs list its real fields](/quest/m2/track-info-docs.md) - Swift, Kotlin and Dart stop naming ordering and latency budget on `TrackInfo`
- [Kotlin dependencies live in a Gradle version catalog](/quest/m2/kt-version-catalog.md) - one catalog entry per library, so a Dependabot bump can't drag another along
- [The dev shell builds Kotlin with Gradle 9](/quest/m2/kt-gradle-9.md) - `gradle_9` in the dev shell, so the Android target builds there
- [The publish support panel lists only codecs publishing accepts](/quest/m2/publish-support-probe.md) - the settings panel runs the publisher's Annex B encoder probe
- [A hung Python test fails instead of hanging the suite](/quest/m2/py-test-timeouts.md) - one pytest-timeout bound covers every `py/` test
