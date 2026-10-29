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

## Required

- [Release execution](/quest/m2/release/README.md) - publication, installed-artifact proof, cleanup, and downstream adoption after m1
- [Video quality](/quest/m2/video-quality/README.md) - a nightly VMAF harness grades every encoder backend and on-demand GoP pops against floors, and moq-boy's encode is fixed under it
- [noq adopts moq-congestion](/quest/m2/noq-congestion.md) - noq's BBR3 and Cubic delegate to the published crate behind noq's unchanged API
- [Byte streams](/quest/m2/stream/README.md) - TCP connections ride MoQ losslessly through relays, proxied per hop rather than as IP packets
- [Mesh](/quest/m2/mesh/README.md) - `moq mesh` enrolls a host or robot and reaches any node by endpoint ID or name for SSH, forwards, and agents
- [P2P](/quest/m2/p2p/README.md) - application-selected peer sessions use shared priorities, routing, and same-epoch handover
- [rust2 publishes its UniFFI fork](/quest/m2/rust2-uniffi.md) - waiting on kixelated/rust2 to import UniFFI and moq's generators and publish them
- [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md) - moq-ffi and every wrapper build from kixelated/rust2's fork under one pin, and moq's generator code is deleted
- [Generated C bindings](/quest/m2/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written crate
- [MoQ on rind](/quest/m2/rind/README.md) - every MoQ crate takes its crypto from rind, a pure-Rust graviola fork, so no build compiles C crypto
- [Benchmark sources](/quest/m2/bench-sources.md) - every measuring nightly benchmark feeds the regression table, and none is a smoke run
- [Poll-native async](/quest/m2/ffi-poll-migrate.md) - moq-ffi's pure-kio async exports poll their state directly instead of going through `ffi::Task`
- [QoS](/quest/m2/qos/README.md) - the relay's egress `lag` histogram and `dropped` counters on the stats totals and prefix tracks
- [Lazy retained HLS instances](/quest/m2/hls-lazy-instances.md) - a replaced HLS instance starts its watchers only when a request reaches it
- [HLS and DASH browser playback](/quest/m2/hls-player-test.md) - hls.js and dash.js play, reload, switch, and survive reconfiguration nightly
- [moq-hls export on kio](/quest/m2/hls-export-kio.md) - export watchers and the init build are owned kio tasks instead of tokio::spawn
- [Re-init retires timelines](/quest/m2/reinit-retire.md) - fMP4 and MKV re-inits retire the old timeline, so import state stays bounded
- [Replaced producers retire their records](/quest/m2/producer-retire.md) - encoders and bindings retire a replaced producer's HLS records
- [Broadcast fairness](/quest/m2/broadcast-fairness.md) - broadcasts share a cluster link fairly, so one tenant's priority never starves another's
- [Cache fair share](/quest/m2/cache-fairness.md) - one tenant cannot crowd another out of the relay's shared group cache
- [Peer grant bound](/quest/m2/peer-grant-bound.md) - a peer's AUTH_OK costs near-linear time in its bytes on every input shape, in Rust and in a browser
- [Plan: watch worker](/quest/m2/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m2/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - H.264 and H.265 from an external Vulkan image on RADV, hand-rolled on ash, proven on an RX 9070
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - an explicit-sync DMA-BUF reaches Intel's encoder, proven on Arrow Lake
- [C++ FFI reuse](/quest/m2/cpp-ffi-reuse.md) - C++ packages link the shared default-feature FFI build once it is proven compatible
- [Upstream reports](/quest/m2/upstream-reports.md) - report the faad, retina, and mr-boxington bugs found before the cut

- [Native compression flush](/quest/m2/native-compression-flush.md) - report the per-frame nonfinal flush requirement in existing browser discussions
- [Native synchronous codecs](/quest/m2/native-compression-sync.md) - report the stateful small-frame synchronous API requirement
- [Native browser codecs](/quest/m2/native-compression-adopt.md) - adopt native operations when timing, frame and resource contracts hold

- [Time test sweep](/quest/m2/time-test-sweep.md) - unrelated test conversion and the workspace lint ratchet
- [Drop test-only cut requests](/quest/m2/video-cut-test-only.md) - moq-video's encode producer loses code only its tests use

- [Pipelined requests](/quest/m2/pipeline-requests/README.md) - SUBSCRIBE and the first FETCH go out with the track-info request at every hop, so first data arrives a round trip sooner per hop
- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - replace FFmpeg video and audio decoding with moq-video and moq-audio, deliver GPU frames, and use native audio/video encoders
- [OBS multitrack](/quest/m2/obs-multitrack.md) - the obs-moq output publishes OBS's native multitrack video encoders as simulcast renditions, configured like OBS multitrack and over obs-websocket
- [MoQ in obs-studio](/quest/m2/obs-studio/README.md) - a native MoQ output and service merged into obs-studio, once its maintainers agree; the plugin carries it until then
- [OBS source renditions](/quest/m2/obs-source-renditions.md) - the OBS source picks the best enabled decodable rendition, using a `preference` the bindings now carry, and resubscribes when a referenced publisher's epoch changes
- [CMake qlog option for OBS](/quest/m2/obs-qlog.md) - an OBS build can opt into qlog trace capture through moq-ffi's `qlog` feature
- [AudioToolbox decode](/quest/m2/audio-decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [AudioToolbox encode](/quest/m2/audio-encode-audiotoolbox.md) - macOS and iOS encode AAC-LC
- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - a released mp4-atom carries the `dOps` channel mapping table, and fMP4 import and export carry it through the OpusHead
- [moqsrc surround Opus](/quest/m2/moqsrc-opus-surround.md) - moqsrc sets the Opus mapping caps, so opusdec plays 3 to 8 channels
- [GPU CI](/quest/m2/gpu-ci.md) - NVIDIA tests gate the merge queue on a self-hosted GPU runner VM, and `just rs nvidia` runs them locally instead of skipping
- [VA-API renderer tests](/quest/m2/vaapi-render-tests.md) - the ignored VA-API renderer import tests run in `just rs gpu` on AMD and Intel and fail instead of skipping
- [Vendor-free CARLA bridge](/quest/m2/carla-bridge.md) - the CARLA demo's GPU bridge lets moq-video pick the encoder, and its stand-in test runs on AMD as well as NVIDIA
- [V4L2 virtual camera](/quest/m2/v4l2-virtual-camera.md) - V4L2 capture runs nightly against the `vivid` virtual driver instead of only on a developer's camera
- [Probe](/quest/m2/probe/README.md) - a session probes up to the bitrate it wants to send or receive, so a stopped simulcast rendition resumes and a viewer up-switches; the browser pads where native sessions retransmit
- [Failed groups reach the publisher](/quest/m2/group-failed.md) - Rust and both JS publishers surface a group whose delivery a subscriber's stream gave up on, with one shape
- [Headless moq-transport groups](/quest/m2/ietf-headless-group.md) - a mid-group stream whose head no fill or joining FETCH supplies is dropped in either arrival order, never kept as a whole group
- [A finished PROBE stops reporting](/quest/m2/probe-end-stale.md) - the Rust receive estimate reads unknown once its PROBE stream ends
- [Media stats](/quest/m2/stats/README.md) - publishers announce a stats track in the catalog, viewers answer a soliciting catalog through a per-catalog `.echo` broadcast, and one model turns both into a health verdict and a preflight report
- [Perf](/quest/m2/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [Wire stats](/quest/m2/wire-stats/README.md) - `Presence` counts each root's transport bytes on the wire, `wire_tx` and `wire_rx`, from QUIC, WebSocket, and embedder gateway sources
- [Performance survey follow-ups](/quest/m2/performance-survey/README.md) - benchmark the source audit's transport, browser, media and serving risks before optimizing
- [Transport x version benches](/quest/m2/bench-transport-sweep.md) - the session benches sweep transports as well as versions
- [Paired bench BASE](/quest/m2/bench-base-paired.md) - `just bench <base>` uses the nightly's paired comparison and the old one is deleted
- [Relay NixOS module](/quest/m2/nix-relay-module.md) - module edits run moq-relay's tests, and `cluster.rootUrl` takes several peers
- [Read-only lookup](/quest/m2/read-only-lookup.md) - a live track lookup stops waking the front and every demand watcher
- [Ordered owed wait](/quest/m2/ordered-owed-wait.md) - an ordered reader waits within max_delay for a missing group only while the track still owes it
- [lite-05/06 gap judgement](/quest/m2/lite-legacy-gap.md) - a resumed copy on lite-05/06 never reads a descending track's newest group as its start
- [Refill evicted groups](/quest/m2/evicted-refill.md) - a group evicted after a reader judged it but before it took it is refilled, never delivered twice, and guarded against thrash
- [Receive pool](/quest/m2/receive-pool.md) - one byte budget per worker covers every connection's receive credit and the cache, billing each frame once
- [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) - measure browser baselines, complete WASM bindings, and adopt Rust behind @moq/net before the later JS-only stage
- [Front parking](/quest/m2/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [Route wakes](/quest/m2/route-wakes.md) - a route change wakes only the fronts it can move, so pool churn stops scaling with served paths
- [Flatten nested Options](/quest/m2/nested-options.md) - three moq-net functions stop building `Option<Option<_>>`, so the TS translation reaches them
- [Fewer allocations in announce fan-out](/quest/m2/announce-fanout-allocs.md) - a deep path costs no more allocations per subscriber than a shallow one
- [Keyed prefix polling](/quest/m2/keyed-prefix-polling.md) - `SubscriberDriver` polls only the prefixes a wake concerns, if a bench shows the slope
- [Route health](/quest/m2/route-health/README.md) - epoch-only active/passive track recovery over metadata-only subscriptions
- [Wrapper traits over moq-net](/quest/m2/net-wrap/README.md) - moq-net exports producer and consumer traits, and every writer and reader above it takes them, so a wrapper such as e2ee passes through unchanged
- [E2EE](/quest/m2/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [iOS capture](/quest/m2/mobile-capture-ios.md) - Rust captures the camera and screen on iOS, setting the catalog rotation
- [Android capture](/quest/m2/mobile-capture-android.md) - Rust captures through NDK/JNI on Android, reusing the existing codecs and setting the catalog rotation
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [Audio join budget](/quest/m2/audio-join-budget.md) - `moq-audio` joining at the latest group loses no group a publisher bursts before its budget widens
- [Latest readers use Range::live()](/quest/m2/readers-range-live.md) - after #6278, readers start at the live edge without waiting on `largest()`
- [Media QA audio cases](/quest/m2/media-qa-audio.md) - AAC, audio codec/rate/channel changes, rendition switches, output mute/volume, and a native publisher's A/V sync are graded like the video cases
- [Audio outlives video](/quest/m2/audio-bufferbloat-av.md) - under bufferbloat audio stays within budget while video may skip, rendered frames stay in sync, and native audio rows are enforced
- [ViSQOL grading](/quest/m2/audio-visqol.md) - the nightly scores played audio against a reference clip and fails below measured floors
- [PulseAudio capture periods](/quest/m2/pulseaudio-capture-period.md) - capture on cpal's PulseAudio host arrives in short periods, not the server's 2 s default fragment, proven on the null sink's monitor
- [Bindings size profile](/quest/m2/ffi-size-profile.md) - a benchmark decides whether the moq-ffi builds ship at opt-level "s", which halves the dylib
- [The catalog publish path compiles once](/quest/m2/catalog-publish-once.md) - the catalog producer and its JSON diff codegen in `moq-mux` instead of once per importer crate, about 1 MiB of the CLI
- [Go module proxy](/quest/m2/go-module-proxy.md) - `moq.dev/moq-ffi` ships from a static GOPROXY we host, so its staticlibs stop growing git history by ~61 MiB per release
- [Go per-platform lib modules](/quest/m2/go-ffi-platform-modules.md) - a Go build downloads only the host's staticlib, and `go mod vendor` works
- [Go cancelled results](/quest/m2/go-cancelled-result.md) - a Go call cancelled as it completes destroys the handle it produced instead of leaving it to the finalizer
- [Dart on iOS](/quest/m2/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [Mobile completion](/quest/m2/mobile-completion.md) - verify the Swift and Kotlin bindings over Rust-owned capture
- [Browser publishing paths under test](/quest/m2/publish-firefox.md) - the media harness publishes from Firefox too, covering the worker capture and video-element fallback Chromium never takes
- [Bindings in the interop matrix](/quest/m2/interop-bindings.md) - Kotlin, Swift, and Dart publish and subscribe against every other interop client
- [WebGPU HDR](/quest/m2/webgpu-hdr.md) - HDR renditions play as HDR where the browser and display can show it, and tone-map to SDR elsewhere
- [HLS restart drain cutoff](/quest/m2/hls-restart-drain-cutoff.md) - a moq-hls recording of a replaced run finishes at a finite boundary while the successor starts at once
- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - the audited moq-net decoders have targets that assert an invariant, and the unsafe peer input reaches runs under Miri
- [Untrusted media parsers have fuzz targets](/quest/m2/media-fuzz.md) - fMP4, MKV, FLV, TS, Annex-B, catalog, and AMF0 parsing fuzzed with a bounded-work invariant
- [Resume as producer replacement](/quest/m2/resume-redesign.md) - route changes replace the writer of one track state, deleting the per-reader resume path in Rust and JS
- [moq-transport requests end through one lease](/quest/m2/ietf-request-lease.md) - every IETF request ends through one polled lease, and the two 18k-line files split by mechanism
- [A moq-transport group head fills its hole in place](/quest/m2/ietf-fill-refill.md) - fills and joining FETCHes write into the tail's group, deleting the `Fill` handoff
- [moq-transport hole markers](/quest/m2/ietf-hole-marker.md) - a negotiated extension names a SUBSCRIBE's holes, so our publishers answer multi-group FETCH from caches that know every group
- [Announce interest streams share one route table](/quest/m2/announce-interest-memory.md) - per-stream announce cost follows what is in flight, not every route
- [One relay serve path for every runtime](/quest/m2/relay-serve-path.md) - io_uring serves through the shared `Connection::serve` and config mapping
- [A parked read of a resumed group reads its budget without re-syncing](/quest/m2/recover-budget.md) - measure, then drop the per-frame reader sync
- [Transcode rungs scale off the runtime and keep bounded state](/quest/m2/transcode-rungs.md) - resize on encoder threads, memoize rungs by shape, one writer per rung track
- [moqsrc owns each rendition pad in one actor](/quest/m2/moqsrc-pad-actor.md) - pad lifecycle stops racing across restarts
- [@moq/publish owns rendition identity in one publication](/quest/m2/publish-publication.md) - one map per broadcast replaces five
- [One port](/quest/m2/one-port/README.md) - one UDP port carries QUIC, SRT, and WebRTC media through an embedder hook, and one TCP port carries HTTP, RTMP, and RTMPS
- [qmux receive leases](/quest/m2/qmux-receive-lease.md) - WebSocket, TCP, and WebRTC sessions bound incomplete messages by their receive window, at a per-session cost the maintainer picks
- [QUIC caps](/quest/m2/rate-quic.md) - paced MAX_DATA credit in and a capped pacer out hold a QUIC session to its token's bitrate
- [WebSocket caps](/quest/m2/rate-websocket.md) - paced reads and writes over bounded socket buffers hold WebSocket to the same caps
- [Publishers learn their cap](/quest/m2/rate-grant.md) - the AUTH grant carries the caps and publishers clamp their encoder to them
- [moq-rtc REMB and TIAS](/quest/m2/rtc-remb-tias.md) - a capped WHIP session steers the browser's encoder under its cap and closes past it
- [moq-rtc pacing](/quest/m2/rtc-pacing.md) - a capped WHEP viewer never receives more than its cap plus burst, NACK resends included
- [SRT rate handle](/quest/m2/srt-rate-handle.md) - a per-connection cap paces SRT pulls, retransmits included, and closes SRT publishers past it
- [One port on the io_uring workers](/quest/m2/uring-demux.md) - `moq-uring`'s workers host the UDP demux, so a ring relay keeps WebRTC media and SRT on its QUIC port
- [Hang contribution references](/quest/m2/contribution-ref.md) - a catalog names a whole contribution broadcast that a player opens only when selected, backing moq.pro's composed rooms
- [Synced data playback](/quest/m2/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [Watch decode gate](/quest/m2/watch-decode-gate.md) - video lookahead stays encoded until it is near presentation, like audio
- [moqsrc A/V sync](/quest/m2/moqsrc-av-sync.md) - a nightly test measures A/V skew from `moqsrc` through clock-synced sinks
- [MP4 export](/quest/m2/mp4-export.md) - `moq export mp4 --output` records crash-safe fragments, then finishes a regular MP4 with moov at the end
- [Importers fill the VR fields](/quest/m2/mux-vr-fields.md) - MP4, Matroska, and Opus imports set the catalog's projection, stereo, and ambisonic layout
- [fMP4 ambisonic layout](/quest/m2/mux-vr-ambisonic-fmp4.md) - fMP4 Opus import fills `ambisonic` from the `dOps` mapping table
- [Linux decoded frames](/quest/m2/obs-decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Windows decoded frames](/quest/m2/obs-decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [macOS GPU input](/quest/m2/obs-macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m2/obs-windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md) - try the production Rust browser implementation as JS-only output and compare source-translator alternatives if needed
- [JS-only @moq/net](/quest/m2/rs2ts-js.md) - adopt the selected Rust-to-JS route after browser parity and measured tradeoffs are accepted
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
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
- [mTLS on tls://](/quest/m2/tls-listener-mtls.md) - a `tls://` listener can identify a cluster peer by its client certificate
- [Link quality](/quest/m2/link-quality.md) - a radio link's cost follows its measured quality without flapping routes
- [io_uring handle lifecycle](/quest/m2/uring-lifecycle.md) - handles fail once their worker is gone, close on last drop like tokio, and surface a close behind unread data
- [io_uring socket teardown](/quest/m2/uring-socket-teardown.md) - socket churn never exhausts buffer-group ids, and a failed receive cancel surfaces
- [Ring drain GOAWAY](/quest/m2/uring-ring-drain.md) - the ring sends moq-transport 17+ sessions their drain GOAWAY
- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - Linux relay packages ship io_uring once the ring is on par with tokio
- [io_uring thread check](/quest/m2/uring-transport-machines.md) - the relay fanout bench measures `moq_uring::transport::Local`'s per-call thread check after the transport is boxed, removing it only if it shows
- [Opus implementation](/quest/m2/audio-opus-backend.md) - compare Opus codec quality, CPU, build cost, and the loss recovery each backend offers
- [Latency ledger](/quest/m2/latency-ledger.md) - a viewer reports its share of playback delay stage by stage: jitter buffer, decode, render, and device
- [Media Foundation decode](/quest/m2/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec on an emulator](/quest/m2/android-mediacodec-ci.md) - moq-video's MediaCodec tests run nightly on an Android emulator, in a lane the audio MediaCodec quests reuse
- [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [NVENC buffer pool](/quest/m2/nvenc-pool.md) - NVENC reuses input and output buffers instead of allocating per frame, if a benchmark shows it wins
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Media Foundation encodes on the texture's adapter](/quest/m2/mf-encode-adapter.md) - Windows machines with two GPUs keep the studio's program on the GPU route into a hardware encoder
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - video with periodic intra refresh imports and plays back cleanly with one group per sweep and a catalog `warmup`
- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer on hardware
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into caller-owned buffers with no copy, once a profile says where
- [BBR media study](/quest/m2/quic-bbr-study.md) - whether Google's growth recognition and precautionary probe stop help media, measured on a shaped media harness
- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - native publishers pick `Realtime`, a receive-ts delay-based controller that keeps the uplink queue near empty for teleoperation
- [Keep-alive from the idle timeout](/quest/m2/quic-keep-alive.md) - the keep-alive interval defaults to a fraction of the negotiated idle timeout; an explicit `quic.keep_alive` overrides
- [Socket close](/quest/m2/quic-socket-close.md) - moq-tokio's QUIC endpoint releases its socket on close, so it drops its wrapper
- [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) - the WGC display and window backend verified on real Windows hardware
- [Windows capture parity](/quest/m2/capture-windows.md) - system and per-app audio and a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture, per-app audio, and explicit display-selection and app-capture limits
- [Per-app audio on macOS](/quest/m2/capture-app-audio-macos.md) - one application's audio through ScreenCaptureKit
- [cpal loads libasound at runtime](/quest/m2/cpal-alsa-runtime.md) - condition: a cpal release whose Linux build carries no load-time libasound requirement
- [Audio capture without ALSA link](/quest/m2/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound, and capture becomes a default feature
- [Ship capture and playback](/quest/m2/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [Egress profile](/quest/m2/quic-egress-profile.md) - measure relay send-path syscalls, pacing bursts, and allocations before optimizing any of them
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - H.265 encode and decode, pre-generated bindings, and pooled resize surfaces, including the moq-dev/vaapi release that carries them
- [GPU capacity and health](/quest/m2/gpu-health.md) - moq-video reports each device's sessions, memory, utilization, and health on NVIDIA, AMD, and Intel alike
- [A release carries multi-vendor GPU input](/quest/m2/gpu-release.md) - `release` carries the GPU surface validated on AMD and Intel; health follows separately
- [The remaining library spawns](/quest/m2/spawn-audit.md) - every async spawn left in a library crate becomes a polled `kio::Tasks` child or is listed with why it stays
- [QUIC on the sim](/quest/m2/quic-sim.md) - MoQ sessions and the relay run over `moq-quic` on the seeded sim, reusing its `Pair` and `Link`
- [Seeded sim](/quest/m2/sim-seed.md) - sim tests draw every jitter from one seeded generator and replay bit-exact

- [Archive time ranges](/quest/m2/archive-time-ranges.md) - a `Time(pts)` range on an archive consumer broadcast resolves from the archive's keys, however old
- [DVR rewind](/quest/m2/archive-dvr.md) - follow-on capability after the release contract is stable
- [An errored track end keeps its history](/quest/m2/errored-end.md) - a replayed archive ends with its source's error after serving every stored group, instead of a clean finish

- [JS HLS field](/quest/m2/js-hls.md) - follow-on capability after the release contract is stable

- [Demo dashboard shows viewer lag](/quest/m2/qos-lag-dashboard.md) - follow-on capability after the release contract is stable

- [Publisher timeliness at relay ingest](/quest/m2/qos-publisher-timeliness.md) - follow-on capability after the release contract is stable

- [Task supervision](/quest/m2/task-supervision.md) - the auth decider loop, stats publisher, and moq-tokio's token and WebSocket internals are supervised like #5447's tasks
- [Auth serve bound address](/quest/m2/auth-serve-bound-addr.md) - `moq auth serve` logs the port it actually bound
- [Relay exposes a verified peer address](/quest/m2/relay-peer-addr.md) - WebRTC and other embedded routes know the client's socket address
- [RTMP client reports a publish or play error](/quest/m2/rtmp-client-error.md) - a refused publish or play surfaces its reason
- [H.264 parsing with maintained crates](/quest/m2/h264-reader.md) - moq-mux parses SPS VUI with h264-reader and shares one bit reader across codec config parsers
- [avcC and hvcC through mp4-atom](/quest/m2/mp4-atom-config.md) - moq-mux stops hand-writing the decoder config records its mp4-atom dependency already parses
- [Relay metrics through prometheus-client](/quest/m2/relay-prometheus-client.md) - `/metrics` renders through the maintained client with the same series
- [JS IETF request streams](/quest/m2/js-request-stream.md) - `RequestStream` owns deadlines and cancellation, and TRACK trailing bytes close the session like Rust
- [moq-transport finish interop](/quest/m2/ietf-finish.md) - the interop `finish` case runs on every even moq-transport draft instead of being skipped
- [Join after a quick finish](/quest/m2/ietf-join-after-finish.md) - a pre-draft-22 joining FETCH that arrives after its subscription finished is still answered, within a bound the peer already negotiates
- [JS joining FETCH](/quest/m2/ietf-js-joining-fetch.md) - the JS publisher answers a joining FETCH that arrives after a quick finish, mirroring the Rust bound
- [ashpd uses maintained D-Bus crates](/quest/m2/ashpd-maintained.md) - wait for a stable zbus 6 port, then remove the portal dependency exceptions
- [Askama uses a maintained TOML parser](/quest/m2/askama-maintained.md) - wait for a stable parser migration, then remove the generator dependency exception
- [Dependency and alternatives audit](/quest/m2/audit-deps.md) - dependencies are current, deduplicated, needed, and the best maintained option
- [Dead code and API surface audit](/quest/m2/audit-dead-code.md) - repeat the removal audit before the next major release
- [Typecheck with bun check](/quest/m2/bun-check.md) - workspace packages typecheck through bun once nixpkgs ships bun 1.4.4
- [Test coverage audit](/quest/m2/audit-test-coverage.md) - repeat the coverage audit against the m1 baselines after the next release ships
- [Documentation audit](/quest/m2/audit-docs.md) - repeat the docs audit after the release ships, against the m1 baselines
- [Test cost audit](/quest/m2/audit-test-cost.md) - the slowest, flakiest, and least valuable tests are fixed, deleted, or planned once the release ships

- [Quest tree audit](/quest/m2/audit-quests.md) - repeat the whole-tree audit after the release ships, against the m1 baseline
- [CI workflow audit](/quest/m2/audit-ci.md) - repeat the per-workflow and per-job review after the release ships, against the m1 baselines
- [Performance audit](/quest/m2/audit-perf.md) - benchmarks that regressed since the m1 run are explained, fixed, or planned, and unbenched hot paths get coverage
- [Simplification audit](/quest/m2/audit-simplify.md) - repeat the simplification audit, starting with the m1 leftovers, after the release ships
- [Upstream unknown-env option in usage-rs](/quest/m2/usage-unknown-env.md) - jdx/usage refuses unknown prefixed env vars, replacing the moq-tokio helper
- [Artifact size audit](/quest/m2/audit-size.md) - repeat the shipping, native, WASM and browser attribution after the release ships
- [Compile time audit](/quest/m2/audit-compile-time.md) - build-time hotspots are measured against the m1 baselines, starting with the transport share now that moq-net compiles once, and the worst are fixed or planned
- [Lint override and deprecation audit](/quest/m2/audit-lints.md) - repeat the suppression and deprecation audit after the release ships, starting with clippy on the macOS and Windows targets
- [Bound cmsg payload reads](/quest/m2/cmsg-decode-bound.md) - `CMsg::decode` refuses a short control message, and Tokio sockets keep the ECN mark
- [Tenant matrix on both runtimes](/quest/m2/tenant-matrix.md) - tenant, mount, HTTP, normalization, and revalidation boundaries are tested on Tokio and io_uring
- [One typed SETUP parse](/quest/m2/setup-parse-once.md) - one function replaces three SETUP parameter parses
- [Archive reads against a real store](/quest/m2/archive-store-bench.md) - LIST/GET counts and latency over track count
- [Quiet logs for clean ends](/quest/m2/quiet-clean-close-logs.md) - clean probe ends and unroutable requests log at debug
- [No MOQ_* knobs outside the binaries](/quest/m2/env-knobs.md) - test, bench, and library knobs leave the `MOQ_*` prefix
- [Clean ALSA playback onset](/quest/m2/audio-alsa-onset.md) - no onset restarts or clicks over PulseAudio
- [Interop audit](/quest/m2/audit-interop.md) - our interop runner entries match what we ship, and every red interop cell with a moq.dev side is fixed, reported, or noted as down or unsupported
- [Open-GOP MPEG-2 export](/quest/m2/mpeg2-open-gop-export.md) - a TS export resuming in an open GOP trims the B-pictures that reference the previous one
