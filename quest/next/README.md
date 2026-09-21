# next

## Goal

The next wave, in priority order: reliability, new capabilities, performance,
and the planning that settles their shared contracts.

## Plan

Nothing here has a branch. Starting a quest moves it under
[main](/quest/main/README.md) or [dev](/quest/dev/README.md), which is where
its target is decided; a questline moves whole. Planning and implementation
are distinct dispatches: a ready planning quest produces decisions, fixtures,
and rewritten implementation quests, not speculative production code. Later work
waits in [future](/quest/future/README.md).

Give one agent ownership of each shared code area at a time (origin/auth, the
JS Reader, audio playback, media containers and archive, bindings, worker
transport, benchmark tooling); worktrees isolate commits, not semantics.

## Quests

- [Origin narrowing](/quest/next/origin-narrowing.md) - a live origin grant narrows in place and ends the subscriptions it no longer covers, the deafen boundary #2714 asked for
- [Relay embedding](/quest/next/relay-embed.md) - an embedder reads the resolved config, reuses the CLI merge, registers listener health, and spawns a test relay without TOML strings
- [Auth embedder](/quest/next/auth-embedder.md) - the lease owns its re-check clock, a gateway session holds a lease, and `Cluster::admit` scopes and tags origins in one call
- [Binding parity](/quest/next/binding-parity.md) - every wrapper reaches every moq-ffi method in its own idiom with moq-net's verbs
- [Binding docs](/quest/next/binding-docs.md) - every binding doc sample names a symbol that exists, checked nightly
- [Gateway embedding](/quest/next/gateway-embed.md) - moq-hls, moq-rtmp, and moq-rtc expose the loop their binaries run to an in-process embedder
- [Catalog consumer](/quest/next/hang-catalog-consumer.md) - reading a catalog is one call in Rust and JS, and JS gains a timeline consumer
- [Ingest source](/quest/next/net-ingest-source.md) - an announce says whether the broadcast entered on this relay or a peer
- [Headless player](/quest/next/watch-player.md) - `Watch.Player` assembles the pipeline the element, the room, and moq.pro each rebuild
- [@moq/net additive](/quest/next/js-net-additive.md) - a live-broadcasts getter, `Table.dynamic`, credential refresh before redial, inferred `share`
- [Last frame duration](/quest/next/mux-last-frame-duration.md) - a group's final frame keeps its duration on 90 kHz and nanosecond imports
- [Binding audio tests](/quest/next/binding-audio-tests.md) - every binding proves the Opus frame duration and throwing setters it exposes, and `smoke --all` publishes audio with an explicit config
- [Decode format](/quest/next/ffi-decode-format.md) - the C-only decode pixel format knob reaches every uniffi binding
- [JSON mutate](/quest/next/json-mutate.md) - Rust gains the closure edit JS already has, beside the guard
- [Publisher clocks](/quest/next/publisher-clock.md) - wire the shared clock through native and browser publisher restarts
- [Broadcast route](/quest/next/js-broadcast-route.md) - JS Announce.Broadcast goes live on any claim matching its path, like Rust routed()
- [io_uring check](/quest/next/check-uring-feature.md) - a moq-relay diff compiles the io-uring feature in `just check`, not only nightly
- [io_uring handshake cancellation](/quest/next/uring-handshake-cancel.md) - dropping a pending handshake releases its connection while the worker keeps running
- [Flaky timing tests](/quest/next/flaky-timing-tests.md) - three real-clock tests become deterministic instead of failing under load
- [Binding stats docs](/quest/next/binding-stats-docs.md) - every binding's doc page lists its connection stats fields with units

- [Audio jitter target](/quest/next/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess
- [Jitter clock](/quest/next/jitter-flush-clock.md) - moq-mux: catalog jitter measures how far behind the media clock an encoder flushes, fed by encoders only, and never decreases
- [Capture denial](/quest/next/browser-permission-qa.md) - moq-publish surfaces a refused camera or microphone instead of retrying forever, and recovers on grant
- [Publisher audio unlock](/quest/next/publish-audio-unlock.md) - the publisher's capture AudioContext is resumed on a gesture or the source is refused, so no silent audio track is announced
- [IETF leftovers](/quest/next/ietf-leftovers.md) - moq-net: the 0x21 priority property, a NOT_SUPPORTED reply to TRACK_STATUS, and the two FETCH refusal codes come from the registry
- [FFI WebSocket fallback](/quest/next/ffi-websocket-fallback.md) - moq-ffi and every wrapper can disable or delay the WebSocket fallback
- [Server cancel](/quest/next/moq-server-close.md) - `MoqServer.cancel` releases the listening socket before returning, so a caller can bind again without retrying
- [Play tune-in backpressure](/quest/next/play-tunein-backpressure.md) - moq play: a tune-in burst larger than the video queue parks the decoder, so the clock never reaches live at a wide `--delay`
- [Play audio rendition gap](/quest/next/play-audio-rendition-gap.md) - moq play: a retired audio rendition drains its sink before the replacement fills one, so the switch costs a `--delay` of silence
- [JavaScript FETCH](/quest/next/js-fetch.md) - generic on-demand group serving and IETF FETCH for browser publishers
- [Archive](/quest/next/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS, on the catalog and store the release ships
- [Wildcard](/quest/next/wildcard/README.md) - a relay resolves subscriptions against advertised prefixes, a service claims the prefix it could serve and refuses the rest instead of enumerating broadcasts, and the browser player treats a covering claim as availability
- [#2152](/quest/next/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - libmoq accepts sessions, the moq-ffi call C still lacks
- [js/publish discontinuity](/quest/next/js-publish-discontinuity.md) - the JS container producer and js/publish emit the same marker group on encoder restart
- [Failure artifacts](/quest/next/qa-failure-artifacts.md) - a failing harness run keeps its run directory and a Playwright trace, and CI uploads them
- [Impaired path](/quest/next/transport-impairment-profile.md) - the transport drills run over a seeded, impaired UDP path on any host
- [Tooling](/quest/next/tooling/README.md) - justfiles become a one-line menu over `sh/`, one impact map scopes CI, and every workflow step runs a recipe
- [Generation](/quest/next/hls-generation.md) - init URLs follow the rendition config and segment URLs carry an embedder-supplied generation, so caching can be re-enabled
- [Path patterns](/quest/next/path-patterns.md) - one matcher for every predicate over broadcast paths: tokens, origins, interest
- [In-band auth](/quest/next/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Stats retier](/quest/next/stats-retier.md) - a re-checked tier retags a live session's stats in place instead of waiting for its next connection
- [io_uring link facts](/quest/next/uring-link-facts.md) - the io_uring workers report a session's peer address and SNI to the auth server like the tokio listener does
- [Decoded frame ownership](/quest/next/decoded-frames.md) - retain moq-video Frames across bindings, with native views or CPU conversion as needed
- [C++ through moq-ffi](/quest/next/cpp/README.md) - generated C++ over moq-ffi with futures and expected-style errors, shipped as a tarball, vcpkg, and Conan, and adopted by the OBS plugin
- [OBS native codecs](/quest/next/obs-moq-video/README.md) - remove FFmpeg decoding dependencies, deliver GPU frames, and use native audio/video encoders
- [Audio codecs](/quest/next/audio-codecs/README.md) - platform audio codecs, explicit unsupported cases, and channel layouts up to 7.1
- [Opus descriptions](/quest/next/audio-opus-input.md) - validate headers and honor codec clock, pre-skip, and gain
- [Capture formats](/quest/next/audio-capture-format.md) - unsupported overrides refuse before device open and channel counts cannot wrap
- [NVENC recovery](/quest/next/nvenc-recovery.md) - partial initialization and rejected rate changes preserve valid state
- [Transcode source](/quest/next/transcode-source.md) - select a rendition the chosen backend can actually decode
- [Keyframe trigger](/quest/next/keyframe-trigger.md) - an application can ask the built-in capture encoder for a keyframe
- [QoS](/quest/next/qos/README.md) - broadcast health: relay starvation and timeliness histograms, and client stats broadcasts from publishers and viewers
- [Drain](/quest/next/drain/README.md) - relay restarts drain sessions over GOAWAY instead of hard-dropping them
- [Transport upgrade](/quest/next/transport-upgrade/README.md) - a session that came up over WebSocket moves to QUIC once the QUIC dial lands, handing over at a group boundary
- [Own the QUIC stack](/quest/next/quic/README.md) - the moq-noq fork carries
  ACK progress, reliable reset, hierarchical scheduling, deadlines, probing,
  keep-alive, peer limits, careful resume, ECN, and qmux
- [P2P](/quest/next/p2p/README.md) - opted-in clients serve each other over data channels and iroh while the relay stays the rendezvous and the fallback, under application policy
- [One port](/quest/next/one-port/README.md) - a relay speaks QUIC, STUN, WebRTC media, and SRT on one UDP port and HTTP, RTMP, and RTMPS on one TCP port
- [Scope track priority](/quest/next/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [Stream sessions](/quest/next/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [IETF on the ring](/quest/next/uring-ietf.md) - the io_uring workers serve moq-transport sessions too, so a uring relay drops no client protocol
- [Perf](/quest/next/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [#2924](/quest/next/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - every listener on both runtimes shares one reloadable served identity, so rotation is atomic and generate works with workers
- [#2964](/quest/next/2964-quic-workers-dropping-one-split-server-resizes-the.md) - integrate the dev worker owner with hardened socket-group formation
- [Audio quality harness](/quest/next/audio-quality-harness/README.md) - a playout latency regression fails a run instead of arriving as a bug report
- [Benchmark comparisons](/quest/next/performance-comparisons.md) - retained evidence, repeated paired runs, and uncertainty for performance claims
- [#3126](/quest/next/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench reports per-interval latency percentiles so the ramp leaves the steady state
- [Relay profiling](/quest/next/performance-profiles.md) - reproducible CPU and allocation captures under the existing workloads
- [Browser benchmarks](/quest/next/browser-benchmarks.md) - measure JS transport, container, decode, and render costs in an identified browser
- [Closure counters](/quest/next/closure-counters.md) - a departed node's return never regresses the closure counters a consumer already saw
- [RTMP interleaving](/quest/next/rtmp-interleaving.md) - isolate partial messages before optimizing assembly copies
- [Relay memory](/quest/next/relay-memory.md) - remeasure what an announcement costs after prefix routes
- [PoP skipping](/quest/next/pop-skipping/README.md) - short cold paths for unpopular broadcasts without losing warm backhaul dedup
- [Route cost in the JS origin](/quest/next/route-cost.md) - the browser origin ranks routes by cost and hops like Rust instead of newest-first
- [Publish channel count](/quest/next/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence
- [JS abandonment](/quest/next/js-subscribe-abandonment.md) - a viewer returning during IETF subscribe setup keeps its track across microtasks
- [IETF stream types](/quest/next/ietf-uni-stream-types.md) - padding streams are discarded stream-only and an unknown uni type closes the session, per draft-21
- [Control timeout code](/quest/next/control-timeout-code.md) - an unanswered control request resets with CONTROL_TIMEOUT on lite and INTERNAL_ERROR on IETF
- [#2991](/quest/next/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - one dynamic producer per track name in both languages, with the sequence namespace surviving a replacement
- [JS track end](/quest/next/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - js/net declares a track end ahead of the live edge and observes the publisher's SUBSCRIBE_END
- [E2EE](/quest/next/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Processor](/quest/next/processor/README.md) - a customer-run worker publishes an on-demand contribution with scoped access
- [#3056](/quest/next/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: the video decoder resets on a declared discontinuity
- [#933](/quest/next/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the catalog rotation follows the live camera's orientation
- [#2075](/quest/next/2075-mirror-catalog-reservation-gating-in-moq-hang-js-hang.md) - @moq/publish gates the first catalog snapshot until every reserved track is described
- [#2848](/quest/next/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [Ladder](/quest/next/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [LOC duration marker](/quest/next/loc-duration-marker.md) - LOC producers write the marker once released consumers skip it
- [#2278](/quest/next/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - hang: expose the fixed catalog-root broadcast clock without synchronizing library playback to wall time
- [Time stretch](/quest/next/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [#2279](/quest/next/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [Caption import](/quest/next/captions-import.md) - fMP4 and MKV subtitle tracks import as text renditions instead of erroring or being dropped
- [MSF caption roles](/quest/next/captions-msf.md) - an MSF caption, subtitle, or sign-language track survives conversion to a hang catalog
- [CEA-608/708](/quest/next/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Colour model](/quest/next/color-model.md) - the catalog describes a rendition's colour and HDR properties instead of leaving a TODO
- [#2067](/quest/next/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - Open-GOP H.264: a regression fixture and a measured cold tune-in
- [Open-GOP leading pictures](/quest/next/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [Catalog warmup](/quest/next/catalog-warmup.md) - `warmup` on video and audio renditions, in the catalog and the draft
- [Audio warmup](/quest/next/audio-warmup.md) - a viewer joining an Opus rendition mid-stream never hears the unconverged first 80 ms
- [#3021](/quest/next/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - GStreamer maps every pad onto one continuous broadcast clock across source restarts
- [#2829](/quest/next/2829-moq-export-ts-the-audio-video-interleave-is-decided-by.md) - moq export ts: the interleave is a media-time watermark bounded by `--max-age`, so two exporters render one broadcast in one order
- [#3489](/quest/next/3489-ts-import-stream-liveness.md) - moq import ts: every elementary stream reports its access units and how long it has been quiet
- [SRT import stats](/quest/next/srt-import-stats.md) - the SRT gateway reports the same per-stream counters instead of nothing
- [Text availability](/quest/next/text-schema.md) - a text track publishes its own coverage index instead of copying the media timeline
- [SRT metadata parity](/quest/next/srt-metadata.md) - the SRT publisher preserves MPEG-TS metadata byte-faithfully like the CLI importer
- [ID3 catalog section](/quest/next/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [fMP4 emsg](/quest/next/emsg.md) - event messages survive fMP4 import, and the timed-metadata contract ID3, SCTE-35, and FLV script tags share is settled with them
- [FLV script tags](/quest/next/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [Release size](/quest/next/release-size.md) - the release scripts' LTO exports become the workspace release profile, and a nightly report shows what each moq-ffi build ships
- [Bindgen CLI split](/quest/next/uniffi-cli-feature.md) - a library build of moq-ffi stops compiling uniffi_bindgen and its 46 crates
- [Dart on iOS](/quest/next/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [libmoq shutdown](/quest/next/libmoq-shutdown.md) - OBS exits cleanly with the plugin loaded: a C ABI `moq_shutdown` stops the libmoq thread before the module is unloaded
- [Kotlin JVM exit](/quest/next/kt-jvm-exit.md) - a Kotlin/JVM program exits cleanly whatever the moq-ffi runtime thread is doing, like Python does since #3766
- [Dart leaks](/quest/next/dart-leak.md) - the generated Dart bindings leak native memory on every call
- [Dart publish](/quest/next/dart-publish.md) - the packages are built and dry-run clean but exist nowhere consumers can install from
- [Dart codec parity](/quest/next/dart-codecs.md) - Dart is the one binding that cannot originate media
- [libmoq fetch](/quest/next/libmoq-fetch.md) - libmoq gains an additive cached-group fetch entry point
- [moq-mux on wasm32](/quest/next/mux-wasm-target.md) - the crate's two wasm blockers are fixed and the target stays in the clippy lane
- [#2850](/quest/next/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - js/net: decode messages synchronously from buffered bytes and delete the publisher read-ahead queue
- [Install moq](/quest/next/moq-installer.md) - one command installs or upgrades the released CLI on macOS and Linux
- [Install URL](/quest/next/moq-install-url.md) - moq.dev serves the canonical installer at /install.sh
- [`moq relay`](/quest/next/moq-relay-subcommand.md) - the relay runs under a `moq` verb with its own flags and TOML, while `moq-relay` stays a minimal binary
- [`moq --listen` admission](/quest/next/cli-serve.md) - a listening CLI session is authenticated, scoped, counted, and drained like a relay's instead of accepting everything
- [#709](/quest/next/709-automatic-letsencrypt-support.md) - the relay provisions and renews its own ACME certificate through rustls-acme over TLS-ALPN-01, persisted on disk
- [Capture without V4L2 bindgen](/quest/next/capture-v4l-bindings.md) - moq-video capture builds on Linux without libclang or kernel headers
- [Audio capture without ALSA link](/quest/next/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound
- [Ship capture and playback](/quest/next/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [io_uring flow control](/quest/next/uring-flow-control-windows.md) - the relay's io_uring workers honor the QUIC flow-control windows instead of refusing them
