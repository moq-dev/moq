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

The first follow-through is [release execution](/quest/m2/release/README.md).
Rate-grant work targets the next unpublished lite version after finalized
lite-07; bitrate enforcement is not part of this release.

Added 2026-10-13 by the redundant TS alignment plan: TS over RTP legs,
ranked just below deterministic TS legs, which it requires.

## Required

- [Release execution](/quest/m2/release/README.md) - publication, installed-artifact proof, cleanup, and downstream adoption after m1
- [P2P](/quest/m2/p2p/README.md) - application-selected peer sessions use shared priorities, routing, and same-epoch handover
- [Epoch anchor](/quest/m2/epoch-anchor.md) - same-epoch importers each derive the catalog clock and timestamp era from the epoch and stay live, so they agree across a 33-bit wrap, a forward clock break, and a restart
- [Raw TS cuts on the epoch timeline](/quest/m2/ts-passthrough-epoch-cuts.md) - passthrough fallback cuts fall on the epoch's grid and the M2ts section derives from the epoch and content, so instances started apart publish identical groups and catalogs
- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - importers sharing one `--epoch` and fed one stream publish identical groups and timestamps, so failover between a redundant pair survives
- [Hard-fork the binding generator](/quest/m2/uniffi-fork.md) - one moq-dev repo holds the chosen generator's core and every language backend, released under one tag and one pin
- [Benchmark trend site](/quest/m2/bench-site.md) - a GitHub Pages site charts the binding, Rust, and JS nightly benchmarks over time
- [Link the benchmark trends](/quest/m2/bench-site-doc.md) - the docs point language choosers at the trend site
- [Payload bytes](/quest/m2/ffi-bytes.md) - payloads cross the FFI in one bulk copy, proven on the binding benchmark in every wrapper
- [Kotlin calls](/quest/m2/kt-ffi-calls.md) - Kotlin calls into moq-ffi cost about what Python's and Go's do, sync and async, once the fork's Kotlin generator drops JNA's per-call overhead
- [Poll-native async](/quest/m2/ffi-poll-migrate.md) - moq-ffi's pure-kio async exports poll their state directly instead of going through `ffi::Task`
- [Foreign poll for hot reads](/quest/m2/ffi-poll-protocol.md) - the fork's generators give each wrapper a sync poll plus a registered waker for track and group reads, so a ready read skips the event-loop round trip and a cancel never loses a frame
- [QoS](/quest/m2/qos/README.md) - broadcast health: relay starvation and timeliness histograms
- [Plan: watch worker](/quest/m2/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m2/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - H.264 and H.265 from an external Vulkan image on RADV, hand-rolled on ash, proven on an RX 9070
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - an explicit-sync DMA-BUF reaches Intel's encoder, proven on Arrow Lake
- [C++ FFI reuse](/quest/m2/cpp-ffi-reuse.md) - C++ packages link the shared default-feature FFI build once it is proven compatible

- [Time test sweep](/quest/m2/time-test-sweep.md) - unrelated test conversion and the workspace lint ratchet

- [Pipelined requests](/quest/m2/pipeline-requests/README.md) - SUBSCRIBE and the first FETCH go out with the track-info request at every hop, so first data arrives a round trip sooner per hop
- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - replace FFmpeg video and audio decoding with moq-video and moq-audio, deliver GPU frames, and use native audio/video encoders
- [OBS multitrack](/quest/m2/obs-multitrack.md) - the obs-moq output publishes OBS's native multitrack video encoders as simulcast renditions, configured like OBS multitrack and over obs-websocket
- [MoQ in obs-studio](/quest/m2/obs-studio/README.md) - a native MoQ output and service merged into obs-studio, once its maintainers agree; the plugin carries it until then
- [AudioToolbox decode](/quest/m2/audio-decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [AudioToolbox encode](/quest/m2/audio-encode-audiotoolbox.md) - macOS and iOS encode AAC-LC
- [mp4-atom dOps mapping](/quest/m2/mp4-atom-dops-mapping.md) - a released mp4-atom reads and writes any `dOps` channel mapping family and table
- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - fMP4 import and export carry an Opus channel mapping table
- [GPU CI](/quest/m2/gpu-ci.md) - NVIDIA tests run nightly on real hardware, and `just rs nvidia` runs them locally instead of skipping
- [Simulcast rung disable](/quest/m2/simulcast-rung-disable.md) - JS, Rust/FFI, and OBS publishers stop encoding the top renditions their grant cannot fund, advertise `enabled: false`, and re-enable with hysteresis
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [Viewer up-switch](/quest/m2/viewer-upswitch.md) - a viewer capped by its small rendition finds headroom through PROBE and moves up
- [Wire stats](/quest/m2/wire-stats/README.md) - `Presence` counts each root's transport bytes on the wire, `wire_tx` and `wire_rx`, from QUIC, WebSocket, and embedder gateway sources
- [Media stats](/quest/m2/stats/README.md) - publishers announce a stats track in the catalog, viewers answer a soliciting catalog through a per-catalog `.echo` broadcast, and one model turns both into a health verdict and a preflight report
- [Perf](/quest/m2/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [Benchmark regressions in CI](/quest/m2/bench-ci.md) - PRs get a non-blocking comparison of the Criterion benches they affect, and a nightly trend on main alerts on regressions
- [Relay session bench](/quest/m2/bench-relay.md) - the same scenario through moq-relay's own connection handling
- [Read-only lookup](/quest/m2/read-only-lookup.md) - a live track lookup stops waking the front and every demand watcher
- [Ordered owed wait](/quest/m2/ordered-owed-wait.md) - an ordered reader waits within max_delay for a missing group only while the track still owes it
- [Unbounded groups](/quest/m2/unbounded-groups/README.md) - groups and frames lose their size caps; the cache sheds parts of a group under pressure and refills the holes in place
- [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) - measure browser baselines, complete WASM bindings, and adopt Rust behind @moq/net before the later JS-only stage
- [Front parking](/quest/m2/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [Route wakes](/quest/m2/route-wakes.md) - a route change wakes only the fronts it can move, so pool churn stops scaling with served paths
- [Route health](/quest/m2/route-health/README.md) - epoch-only active/passive track recovery over metadata-only subscriptions
- [E2EE](/quest/m2/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [iOS capture](/quest/m2/mobile-capture-ios.md) - Rust captures the camera and screen on iOS, setting the catalog rotation
- [Android capture](/quest/m2/mobile-capture-android.md) - Rust captures through NDK/JNI on Android, reusing the existing codecs and setting the catalog rotation
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [Audio join budget](/quest/m2/audio-join-budget.md) - `moq-audio` joining at the latest group loses no group a publisher bursts before its budget widens
- [Bindings size profile](/quest/m2/ffi-size-profile.md) - a benchmark decides whether the moq-ffi builds ship at opt-level "s", which halves the dylib
- [Zero-copy byte returns](/quest/m2/ffi-bytes-return.md) - returned payloads reach the host without a copy, if the payload numbers justify it
- [Namespaced external types](/quest/m2/uniffi-namespaces.md) - the UniFFI fork lets two crates export the same type name, reopening the per-layer crate split
- [Go module proxy](/quest/m2/go-module-proxy.md) - `moq.dev/moq-ffi` ships from a static GOPROXY we host, so its staticlibs stop growing git history by ~61 MiB per release
- [Go per-platform lib modules](/quest/m2/go-ffi-platform-modules.md) - a Go build downloads only the host's staticlib, and `go mod vendor` works
- [Dart on iOS](/quest/m2/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [Mobile completion](/quest/m2/mobile-completion.md) - verify the Swift and Kotlin bindings over Rust-owned capture before closing #700
- [WebGPU HDR](/quest/m2/webgpu-hdr.md) - HDR renditions play as HDR where the browser and display can show it, and tone-map to SDR elsewhere
- [Grant stats label](/quest/m2/grant-label.md) - an auth grant's optional `label` names what its session's presence and live bytes count under in stats, apart from the path scope it may access
- [RTSP import](/quest/m2/rtsp-import.md) - `moq import rtsp` publishes an IP camera from its own network, through a `moq-rtsp` crate whose one-session ingest a caller can supervise itself
- [One port](/quest/m2/one-port/README.md) - one UDP port carries QUIC, SRT, and WebRTC media through an embedder hook, and one TCP port carries HTTP, RTMP, and RTMPS
- [QUIC caps](/quest/m2/rate-quic.md) - paced MAX_DATA credit in and a capped pacer out hold a QUIC session to its token's bitrate
- [WebSocket caps](/quest/m2/rate-websocket.md) - paced reads and writes over bounded socket buffers hold WebSocket to the same caps
- [Publishers learn their cap](/quest/m2/rate-grant.md) - the AUTH grant carries the caps and publishers clamp their encoder to them
- [One port on the io_uring workers](/quest/m2/uring-demux.md) - `moq-uring`'s workers host the UDP demux, so a ring relay keeps WebRTC media and SRT on its QUIC port
- [Synced data playback](/quest/m2/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [Watch decode gate](/quest/m2/watch-decode-gate.md) - video lookahead stays encoded until it is near presentation, like audio
- [Deterministic TS legs](/quest/m2/ts-hitless.md) - two `--sync` legs of one broadcast render every common slot identically, continuity counters included, through complementary loss
- [TS over RTP legs](/quest/m2/ts-rtp-egress.md) - two legs send identical RTP packets, headers included, so an ST 2022-7 receiver can merge them
- [DVB E-AC-3](/quest/m2/ts-eac3.md) - E-AC-3 private data is split per access unit (independent frame plus its dependent substreams) and buffer-modelled in TS export
- [moqsrc A/V sync](/quest/m2/moqsrc-av-sync.md) - a nightly test measures A/V skew from `moqsrc` through clock-synced sinks
- [MP4 export](/quest/m2/mp4-export.md) - `moq export mp4 --output` records crash-safe fragments, then finishes a regular MP4 with moov at the end
- [fMP4 edit lists](/quest/m2/fmp4-edit-lists.md) - the fMP4 importer applies edit lists to frame timestamps
- [Linux decoded frames](/quest/m2/obs-decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Windows decoded frames](/quest/m2/obs-decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [macOS GPU input](/quest/m2/obs-macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m2/obs-windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md) - try the production Rust browser implementation as JS-only output and compare source-translator alternatives if needed
- [JS-only @moq/net](/quest/m2/rs2ts-js.md) - adopt the selected Rust-to-JS route after browser parity and measured tradeoffs are accepted
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
- [qmux on the QUIC stream state machine](/quest/m2/quic-qmux.md) - qmux is a
  first-class crate over `moq-quic`'s shared stream state machine
- [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) - every backend implements moq-net's `transport::poll` traits, and the generic `web_transport_trait` adapters are deleted
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Per-stream ACK progress](/quest/m2/quic-ack-progress.md) - `moq-quic` reports
  how far a send stream has been acknowledged and when
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - the
  backend-neutral hook on moq-net's own transport trait that awaits an
  acknowledged stream offset, implemented by the moq-tokio and moq-uring adapters
- [Starvation at frame granularity](/quest/m2/starvation-frames.md) - the
  acknowledged frontier moves at every frame boundary through `poll_acked`,
  with a delivery-delay histogram for jitter
- [#3199](/quest/m2/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup, if a profile shows the win
- [#3200](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [Browser media report](/quest/m2/rs2ts-browser-report.md) - compare Rust-backed and handwritten @moq/net through full media workloads, supplementing the pre-adoption comparisons
- [mTLS on tls://](/quest/m2/tls-listener-mtls.md) - a `tls://` listener can identify a cluster peer by its client certificate
- [Link quality](/quest/m2/link-quality.md) - a radio link's cost follows its measured quality without flapping routes
- [Archive recovery listing](/quest/m2/archive-recovery-listing.md) - a resumed DVR lists what changed since its checkpoint, not every stored group
- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - Linux relay packages ship io_uring once the ring is on par with tokio
- [Opus implementation](/quest/m2/audio-opus-backend.md) - compare Opus codec quality, CPU, build cost, and the loss recovery each backend offers
- [Latency ledger](/quest/m2/latency-ledger.md) - a viewer reports its share of playback delay stage by stage: jitter buffer, decode, render, and device
- [Media Foundation decode](/quest/m2/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [NVENC buffer pool](/quest/m2/nvenc-pool.md) - NVENC reuses input and output buffers instead of allocating per frame, if a benchmark shows it wins
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - video with periodic intra refresh imports and plays back cleanly with one group per sweep and a catalog `warmup`
- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer on hardware
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into caller-owned buffers with no copy, once a profile says where
- [BBR media study](/quest/m2/quic-bbr-natural-drain.md) - whether bounded drain credit avoids ProbeRTT deadline interference, and where our BBR differs from Google's
- [Keep-alive from the idle timeout](/quest/m2/quic-keep-alive.md) - the keep-alive interval defaults to a fraction of the negotiated idle timeout; an explicit `quic.keep_alive` overrides
- [Socket close](/quest/m2/noq-socket-close.md) - moq-tokio's QUIC endpoint releases its socket on close, so it drops its wrapper
- [TS health stats](/quest/m2/ts-health-stats.md) - the TS counters ride the stats plumbing beside the media counters
- [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) - the WGC display and window backend verified on real Windows hardware
- [Windows capture parity](/quest/m2/capture-windows.md) - system audio and a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture with explicit display-selection and app-capture limits
- [cpal loads libasound at runtime](/quest/m2/cpal-alsa-runtime.md) - condition: a cpal release whose Linux build carries no load-time libasound requirement
- [Audio capture without ALSA link](/quest/m2/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound, and capture becomes a default feature
- [Ship capture and playback](/quest/m2/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [Egress profile](/quest/m2/quic-egress-profile.md) - measure relay send-path syscalls, pacing bursts, and allocations before optimizing any of them
- [Compressed tracks](/quest/m2/flate.md) - moq-ffi and every wrapper expose flate tracks through a `flate` namespace like `json`
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - H.265 encode and decode, pre-generated bindings, and pooled resize surfaces, including the moq-dev/vaapi release that carries them
- [GPU capacity and health](/quest/m2/gpu-health.md) - moq-video reports each device's sessions, memory, utilization, and health on NVIDIA, AMD, and Intel alike
- [A release carries multi-vendor GPU input](/quest/m2/gpu-release.md) - `release` carries the GPU surface validated on AMD and Intel; health follows separately
- [Runtime trait](/quest/m2/runtime-trait.md) - one runtime trait for spawn and time replaces web-async, with tokio, browser, io_uring, and sim implementations
- [QUIC on the sim](/quest/m2/quic-sim.md) - the sim drives `moq-quic` over in-memory datagrams with latency and loss
- [Seeded sim](/quest/m2/sim-seed.md) - sim tests draw every jitter from one seeded generator and replay bit-exact

- [DVR rewind](/quest/m2/archive-dvr.md) - follow-on capability after the release contract is stable

- [JS per-track timelines](/quest/m2/archive-js-timelines.md) - follow-on capability after the release contract is stable

- [Demo dashboard shows viewer lag](/quest/m2/qos-lag-dashboard.md) - follow-on capability after the release contract is stable

- [Publisher timeliness at relay ingest](/quest/m2/qos-publisher-timeliness.md) - follow-on capability after the release contract is stable

- [Task supervision](/quest/m2/task-supervision.md) - the auth decider loop, stats publisher, and moq-tokio's token and WebSocket internals are supervised like #5447's tasks
- [Auth serve bound address](/quest/m2/auth-serve-bound-addr.md) - `moq auth serve` logs the port it actually bound
- [Relay exposes a verified peer address](/quest/m2/relay-peer-addr.md) - WebRTC and other embedded routes know the client's socket address
- [RTMP client reports a publish or play error](/quest/m2/rtmp-client-error.md) - a refused publish or play surfaces its reason
- [Adopt usage-rs 7](/quest/m2/usage-rs-7.md) - the workspace builds on usage-rs 7
