# m1: next wave

## Goal

The next wave, in priority order: reliability, new capabilities, performance,
and the planning that settles their shared contracts.

## Plan

Planning and implementation are distinct dispatches: a ready planning quest
produces decisions, fixtures, and rewritten implementation quests, not
speculative production code. Later work waits in [m2](/quest/m2/README.md).

Give one agent ownership of each shared code area at a time (origin/auth, the
JS Reader, audio playback, media containers and archive, bindings, worker
transport, benchmark tooling); worktrees isolate commits, not semantics.

## Quests

- [Play harness](/quest/m1/play-harness.md) - moq play's tune-in, rendition-switch, and drain logic runs in per-PR CI without a device
- [Missing fetch group](/quest/m1/fetch-missing-group.md) - HTTP /fetch answers 404 and `moq fetch` fails cleanly for a group the track lacks
- [libmoq hidden opt-in](/quest/m1/libmoq-hidden.md) - `moq_origin_announced` takes a `hidden` flag so C callers can list `.`-named broadcasts
- [lite-07 stream count](/quest/m1/lite-stream-count.md) - moq-lite-07 replaces SUBSCRIBE_DROP with a group-stream count in SUBSCRIBE_END, like moq-transport
- [Announce compression](/quest/m1/announce-compression.md) - a lite-07 announce reuses the path head and hop-chain tail of a live announcement on its stream instead of resending them
- [Rust track tail](/quest/m1/rust-track-tail.md) - a moq-net subscriber accepts groups that arrive after the subscription's end, and PublishDone carries the real stream count
- [Session death error](/quest/m1/session-death-error.md) - a dying session ends its tracks with its own error in Rust and JS, never a clean end, `Dropped`, or `Cancel`
- [Signal.race cleanup](/quest/m1/signal-race.md) - `Signal.race` releases its signal listeners when its result loses a race
- [Origin narrowing](/quest/m1/origin-narrowing.md) - a live origin grant narrows in place and ends the subscriptions it no longer covers, the deafen boundary #2714 asked for
- [Auth expiry clock](/quest/m1/auth-expiry-clock.md) - moq-auth and the relay hold one fixed expiry deadline and honour the same skew allowance
- [Binding surface](/quest/m1/binding-surface.md) - moq-ffi, libmoq, and every wrapper expose the decode delay, route source, and connection timing
- [FFI shape](/quest/m1/ffi-shape/README.md) - the bindings mirror Rust's layers: net at the root, then media, json, audio, and video namespaces built from the handle below
- [Track demand](/quest/m1/track-demand.md) - Rust and JS watch a track's subscribers through `demand()` alone
- [Broadcast close](/quest/m1/broadcast-close/README.md) - `close()` is the one way to end a broadcast in every language, a permanent retraction that leaves in-flight tracks alone
- [Relay peer set](/quest/m1/relay-peer-set.md) - a wire consumer tells a client hop from a peer hop, and every mesh credential can mark a peer
- [CLI import clock](/quest/m1/cli-import-clock.md) - fMP4, TS, and FLV imports publish on the shared broadcast clock across restarts
- [Native clock fixtures](/quest/m1/native-clock-fixtures.md) - CI drives native capture through clock edge cases and asserts the published timestamps
- [CLI inspection](/quest/m1/cli-inspect/README.md) - `moq ls` lists what is live and `moq fetch` reads a group over MoQ, and a guide shows how to inspect a relay
- [JS caught up](/quest/m1/js-announce-caught-up.md) - @moq/net's announce consumer says when the initial set has landed, like Rust
- [Bindings caught up](/quest/m1/announce-live-bindings.md) - moq-ffi, libmoq, and every wrapper yield the same flat announce event, `Live` included
- [Max age over EXPIRES](/quest/m1/ietf-max-age.md) - max age defaults to 0, is set only by the publisher, and crosses moq-transport as EXPIRES
- [IETF announce count](/quest/m1/ietf-announce-count.md) - an opt-in moq-transport extension carries the replay count, so IETF announce consumers go live without a timer

- [Jitter clock](/quest/m1/jitter-flush-clock.md) - renditions advertise `delay` (lag behind the earliest track) and `jitter` (spread), measured at encoder flush, never lowered; js/watch sizes playout over what it subscribes
- [Data jitter](/quest/m1/data-jitter.md) - JSON and binary tracks with a capture time advertise a detected `delay` and `jitter`
- [Play tune-in backpressure](/quest/m1/play-tunein-backpressure.md) - moq play: a tune-in burst larger than the video queue parks the decoder, so the clock never reaches live at a wide `--delay`
- [JavaScript FETCH](/quest/m1/js-fetch.md) - generic on-demand group serving and IETF FETCH for browser publishers
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS, on the catalog and store the release ships
- [Wildcard](/quest/m1/wildcard/README.md) - a relay resolves subscriptions against advertised prefixes, a service claims the prefix it could serve and refuses the rest instead of enumerating broadcasts, and the browser player treats a covering claim as availability
- [Tooling](/quest/m1/tooling/README.md) - justfiles become a one-line menu over `sh/`, one impact map scopes CI, and every workflow step runs a recipe
- [Path patterns](/quest/m1/path-patterns.md) - one matcher for every predicate over broadcast paths: tokens, origins, interest
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Decoded frame ownership](/quest/m1/decoded-frames.md) - retain moq-video Frames across bindings, with native views or CPU conversion as needed
- [C++ through moq-ffi](/quest/m1/cpp/README.md) - generated C++ over moq-ffi with futures and expected-style errors, shipped as a tarball, vcpkg, and Conan, and adopted by the OBS plugin
- [OBS native codecs](/quest/m1/obs-moq-video/README.md) - remove FFmpeg decoding dependencies, deliver GPU frames, and use native audio/video encoders
- [Audio codecs](/quest/m1/audio-codecs/README.md) - platform audio codecs, explicit unsupported cases, and channel layouts up to 7.1
- [Opus descriptions](/quest/m1/audio-opus-input.md) - validate headers and honor codec clock, pre-skip, and gain
- [Capture formats](/quest/m1/audio-capture-format.md) - unsupported overrides refuse before device open and channel counts cannot wrap
- [NVENC teardown](/quest/m1/nvenc-teardown.md) - a rejected NVENC encode no longer hangs process shutdown
- [NVENC recovery](/quest/m1/nvenc-recovery.md) - partial initialization and rejected rate changes preserve valid state
- [GPU pool reservation](/quest/m1/gpu-pool-reservation.md) - a full GPU frame pool is a `None` reservation the caller drops on, not an error to match
- [Transcode source](/quest/m1/transcode-source.md) - select a rendition the chosen backend can actually decode
- [Keyframe trigger](/quest/m1/keyframe-trigger.md) - an application can ask the built-in capture encoder for a keyframe
- [QoS](/quest/m1/qos/README.md) - broadcast health: relay starvation and timeliness histograms, and client stats broadcasts from publishers and viewers
- [Drain](/quest/m1/drain/README.md) - relay restarts drain sessions over GOAWAY instead of hard-dropping them
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - a session that came up over WebSocket moves to QUIC once the QUIC dial lands, handing over at a group boundary
- [Own the QUIC stack](/quest/m1/quic/README.md) - the moq-noq fork carries
  ACK progress, reliable reset, hierarchical scheduling, deadlines, probing,
  keep-alive, peer limits, careful resume, ECN, and qmux
- [P2P](/quest/m1/p2p/README.md) - opted-in clients serve each other over data channels and iroh while the relay stays the rendezvous and the fallback, under application policy
- [One port](/quest/m1/one-port/README.md) - a relay speaks QUIC, STUN, WebRTC media, and SRT on one UDP port and HTTP, RTMP, and RTMPS on one TCP port
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [Stream sessions](/quest/m1/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [IETF on the ring](/quest/m1/uring-ietf.md) - the io_uring workers serve moq-transport sessions too, so a uring relay drops no client protocol
- [Perf](/quest/m1/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [#2924](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - every listener on both runtimes shares one reloadable served identity, so rotation is atomic and generate works with workers
- [#2964](/quest/m1/2964-quic-workers-dropping-one-split-server-resizes-the.md) - integrate the dev worker owner with hardened socket-group formation
- [Audio quality harness](/quest/m1/audio-quality-harness/README.md) - a playout latency regression fails a run instead of arriving as a bug report
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - PRs get a non-blocking comparison of the Criterion benches they affect, and a nightly trend on main alerts on regressions
- [Benchmark comparisons](/quest/m1/performance-comparisons.md) - retained evidence, repeated paired runs, and uncertainty for performance claims
- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench reports per-interval latency percentiles so the ramp leaves the steady state
- [Sans-IO session bench](/quest/m1/bench-session.md) - a moq-net bench drives publisher, relay, and subscribers over the in-memory transport, swept over publishers, subscribers, and frame size
- [Relay session bench](/quest/m1/bench-relay.md) - the same scenario through moq-relay's own connection handling
- [Bench coverage](/quest/m1/bench-coverage.md) - Criterion targets for moq-mux containers, the hang catalog, moq-auth verification, and moq-pattern matching
- [Relay profiling](/quest/m1/performance-profiles.md) - reproducible CPU and allocation captures under the existing workloads
- [Browser benchmarks](/quest/m1/browser-benchmarks.md) - measure JS transport, container, decode, and render costs in an identified browser
- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m1/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Closure counters](/quest/m1/closure-counters.md) - a departed node's return never regresses the closure counters a consumer already saw
- [RTMP interleaving](/quest/m1/rtmp-interleaving.md) - isolate partial messages before optimizing assembly copies
- [Relay memory](/quest/m1/relay-memory.md) - remeasure what an announcement costs after prefix routes
- [PoP skipping](/quest/m1/pop-skipping/README.md) - short cold paths for unpopular broadcasts without losing warm backhaul dedup
- [Route cost in the JS origin](/quest/m1/route-cost.md) - the browser origin ranks routes by cost and hops like Rust instead of newest-first
- [Front parking](/quest/m1/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence
- [JS abandonment](/quest/m1/js-subscribe-abandonment.md) - a viewer returning during IETF subscribe setup keeps its track across microtasks
- [IETF stream types](/quest/m1/ietf-uni-stream-types.md) - padding streams are discarded stream-only and an unknown uni type closes the session, per draft-21
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - one dynamic producer per track name in both languages, with the sequence namespace surviving a replacement
- [Epoch primitive](/quest/m1/epoch.md) - one `Epoch` type in moq-net and @moq/net, carried as a trailing `@<uuidv7>` path segment, shared by e2ee and broadcast epochs
- [E2EE](/quest/m1/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - each publish of a name gets a fresh `@<uuidv7>` epoch, viewers follow the newest live one at once, and bare names still resolve on every version
- [Processor](/quest/m1/processor/README.md) - a customer-run worker publishes an on-demand contribution with scoped access
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: the video decoder resets on a declared discontinuity
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the catalog rotation follows the live camera's orientation
- [#2075](/quest/m1/2075-mirror-catalog-reservation-gating-in-moq-hang-js-hang.md) - @moq/publish gates the first catalog snapshot until every reserved track is described
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [Ladder](/quest/m1/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [LOC duration marker](/quest/m1/loc-duration-marker.md) - LOC producers write the marker once released consumers skip it
- [#2278](/quest/m1/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - hang: expose the fixed catalog-root broadcast clock without synchronizing library playback to wall time
- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [#2279](/quest/m1/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [Caption import](/quest/m1/captions-import.md) - fMP4 and MKV subtitle tracks import as text renditions instead of erroring or being dropped
- [MSF caption roles](/quest/m1/captions-msf.md) - an MSF caption, subtitle, or sign-language track survives conversion to a hang catalog
- [CEA-608/708](/quest/m1/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Colour model](/quest/m1/color-model.md) - the catalog describes a rendition's colour and HDR properties instead of leaving a TODO
- [#2067](/quest/m1/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - Open-GOP H.264: a regression fixture and a measured cold tune-in
- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [Catalog warmup](/quest/m1/catalog-warmup.md) - `warmup` on video and audio renditions, in the catalog and the draft
- [Audio warmup](/quest/m1/audio-warmup.md) - a viewer joining an Opus rendition mid-stream never hears the unconverged first 80 ms
- [#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - GStreamer maps every pad onto one continuous broadcast clock across source restarts
- [Export linger](/quest/m1/export-linger.md) - every `moq export` waits `--linger` for a broadcast to return, and exits 0 on a clean end and 1 on a drop
- [TS byte schedule](/quest/m1/ts-export-byte-schedule.md) - moq export ts places PCRs and padding on the byte grid `mpegts.muxRate` implies, so a receiver can clock off arrival
- [#3489](/quest/m1/3489-ts-import-stream-liveness.md) - moq import ts: every elementary stream reports its access units and how long it has been quiet
- [SRT import stats](/quest/m1/srt-import-stats.md) - the SRT gateway reports the same per-stream counters instead of nothing
- [Text availability](/quest/m1/text-schema.md) - a text track publishes its own coverage index instead of copying the media timeline
- [ID3 catalog section](/quest/m1/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [fMP4 emsg](/quest/m1/emsg.md) - event messages survive fMP4 import, and the timed-metadata contract ID3, SCTE-35, and FLV script tags share is settled with them
- [FLV script tags](/quest/m1/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [Release size](/quest/m1/release-size.md) - the release scripts' LTO exports become the workspace release profile, and a nightly report shows what each moq-ffi build ships
- [Dart on iOS](/quest/m1/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [libmoq shutdown](/quest/m1/libmoq-shutdown.md) - OBS exits cleanly with the plugin loaded: a C ABI `moq_shutdown` stops the libmoq thread before the module is unloaded
- [Kotlin JVM exit](/quest/m1/kt-jvm-exit.md) - a Kotlin/JVM program exits cleanly whatever the moq-ffi runtime thread is doing, like Python does since #3766
- [Dart publish](/quest/m1/dart-publish.md) - the packages are built and dry-run clean but exist nowhere consumers can install from
- [Dart codec parity](/quest/m1/dart-codecs.md) - Dart is the one binding that cannot originate media
- [libmoq fetch](/quest/m1/libmoq-fetch.md) - libmoq gains an additive cached-group fetch entry point
- [moq-mux on wasm32](/quest/m1/mux-wasm-target.md) - the crate's two wasm blockers are fixed and the target stays in the clippy lane
- [#2850](/quest/m1/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - js/net: decode messages synchronously from buffered bytes and delete the publisher read-ahead queue
- [Install moq](/quest/m1/moq-installer.md) - one command installs or upgrades the released CLI on macOS and Linux
- [Install URL](/quest/m1/moq-install-url.md) - moq.dev serves the canonical installer at /install.sh
- [`moq relay`](/quest/m1/moq-relay-subcommand.md) - the relay runs under a `moq` verb with its own flags and TOML, while `moq-relay` stays a minimal binary
- [`moq --listen` admission](/quest/m1/cli-serve.md) - a listening CLI session is authenticated, scoped, counted, and drained like a relay's instead of accepting everything
- [#709](/quest/m1/709-automatic-letsencrypt-support.md) - the relay provisions and renews its own ACME certificate through rustls-acme over TLS-ALPN-01, persisted on disk
- [Audio capture without ALSA link](/quest/m1/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound
- [Ship capture and playback](/quest/m1/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [io_uring flow control](/quest/m1/uring-flow-control-windows.md) - the relay's io_uring workers honor the QUIC flow-control windows instead of refusing them
- [Remove effect.cancel](/quest/m1/effect-cancel.md) - `@moq/signals` drops the deprecated `Effect.cancel` on dev
