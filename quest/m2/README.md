# m2: next wave

## Goal

The next agent wave across reliability, new capabilities, performance, and
planning. A quest can stay here with unresolved decisions when planning it
is part of this wave; settle shared contracts before dependent implementation.

## Plan

Keep a broad mix of work, ordered by expected demand. Planning and
implementation are distinct dispatch choices; the milestone alone does not
mean a quest is ready for implementation. The questlines migrated from the
downstream moq.pro tree keep their internal priority; their product halves (pins,
dashboards, fleet rollout) stay downstream.

A few entries are defects rather than capabilities: an m0 quest whose blocker
is an unreleased dependency, or whose symptom nobody is hitting, sits next to
the feature it shares code with instead of holding a rank in m0 that nothing
can act on. Each still carries its own plan and regression test.

Work that builds on dev-only code but breaks nothing and gates nothing (the
io_uring stream sessions, the perf line, the QUIC worker quests)
also sits here and starts on `main` after the dev merge. The four media crates'
pre-0.1 contracts are the explicit exception in [m0](/quest/m0/README.md);
their API-preserving implementation and performance follow-ups remain here.
Other `0.0.x` work stays here. The token SDK default switch explicitly targets a
subsequent breaking dev cycle after v1 readers exist; it is not additive M2
work for main.

Fleet dispatch uses `quest ready` after checking whether a branch or PR already
claims the quest. A ready planning quest produces decisions, fixtures, and
rewritten implementation quests; it does not authorize speculative production
code. Audio jitter specification, metadata association, capture ergonomics,
and WebTransport open-contract work are planning dispatches.

Land M1 public contracts and merge dev into main before dispatching their
M2 implementation dependents. Those implementations require the merge quest
explicitly, so completing an API quest on dev cannot release them early.
Within the ready set, give one agent ownership of each shared code area at a
time: origin/auth, JS Reader, audio playback, media container/archive, bindings,
worker transport, and benchmark tooling. Separate worktrees isolate commits,
but do not remove semantic overlap. Required links order the known overlaps:
Reader decoding before buffering, script relocation before benchmark comparisons
before profiling, shared frame backing before adapters, and metadata association
before format-specific metadata. Unrelated areas can proceed in parallel.

## Quests

- [Origin narrowing](/quest/m2/origin-narrowing.md) - a live origin grant narrows in place and ends the subscriptions it no longer covers, the deafen boundary #2714 asked for
- [Relay embedding](/quest/m2/relay-embed.md) - an embedder reads the resolved config, reuses the CLI merge, registers listener health, and spawns a test relay without TOML strings
- [Auth embedder](/quest/m2/auth-embedder.md) - the lease owns its re-check clock, a gateway session holds a lease, and `Cluster::admit` scopes and tags origins in one call
- [Binding parity](/quest/m2/binding-parity.md) - every wrapper reaches every moq-ffi method in its own idiom with moq-net's verbs
- [Binding docs](/quest/m2/binding-docs.md) - every binding doc sample names a symbol that exists, checked nightly
- [Gateway embedding](/quest/m2/gateway-embed.md) - moq-hls, moq-rtmp, and moq-rtc expose the loop their binaries run to an in-process embedder
- [Catalog consumer](/quest/m2/hang-catalog-consumer.md) - reading a catalog is one call in Rust and JS, and JS gains a timeline consumer
- [Ingest source](/quest/m2/net-ingest-source.md) - an announce says whether the broadcast entered on this relay or a peer
- [Headless player](/quest/m2/watch-player.md) - `Watch.Player` assembles the pipeline the element, the room, and moq.pro each rebuild
- [@moq/net additive](/quest/m2/js-net-additive.md) - a live-broadcasts getter, `Table.dynamic`, credential refresh before redial, inferred `share`
- [Last frame duration](/quest/m2/mux-last-frame-duration.md) - a group's final frame keeps its duration on 90 kHz and nanosecond imports
- [Snapshot clobber](/quest/m2/json-modify-clobber.md) - a snapshot edit fails on a shape mismatch instead of seeding a default
- [Binding audio tests](/quest/m2/binding-audio-tests.md) - every binding proves the Opus frame duration and throwing setters it exposes, and smoke-full publishes audio with an explicit config
- [Decode format](/quest/m2/ffi-decode-format.md) - the C-only decode pixel format knob reaches every uniffi binding
- [JSON mutate](/quest/m2/json-mutate.md) - Rust gains the closure edit JS already has, beside the guard
- [Publisher clocks](/quest/m2/publisher-clock.md) - wire the shared clock through native and browser publisher restarts
- [Broadcast route](/quest/m2/js-broadcast-route.md) - JS Announce.Broadcast goes live on any claim matching its path, like Rust routed()
- [io_uring check](/quest/m2/check-uring-feature.md) - a moq-relay diff compiles the io-uring feature in `just check`, not only nightly
- [io_uring handshake cancellation](/quest/m2/uring-handshake-cancel.md) - dropping a pending handshake releases its connection while the worker keeps running
- [Flaky timing tests](/quest/m2/flaky-timing-tests.md) - three real-clock tests become deterministic instead of failing under load
- [Binding stats docs](/quest/m2/binding-stats-docs.md) - every binding's doc page lists its connection stats fields with units

- [Audio jitter target](/quest/m2/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess
- [A/V clock](/quest/m2/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Jitter clock](/quest/m2/jitter-flush-clock.md) - moq-mux: catalog jitter measures how far behind the media clock an encoder flushes, fed by encoders only, and never decreases
- [Capture denial](/quest/m2/browser-permission-qa.md) - moq-publish surfaces a refused camera or microphone instead of retrying forever, and recovers on grant
- [Publisher audio unlock](/quest/m2/publish-audio-unlock.md) - the publisher's capture AudioContext is resumed on a gesture or the source is refused, so no silent audio track is announced
- [IETF leftovers](/quest/m2/ietf-leftovers.md) - moq-net: the 0x21 priority property, a NOT_SUPPORTED reply to TRACK_STATUS, and the two FETCH refusal codes come from the registry
- [FFI WebSocket fallback](/quest/m2/ffi-websocket-fallback.md) - moq-ffi and every wrapper can disable or delay the WebSocket fallback
- [Server cancel](/quest/m2/moq-server-close.md) - `MoqServer.cancel` releases the listening socket before returning, so a caller can bind again without retrying
- [Play tune-in backpressure](/quest/m2/play-tunein-backpressure.md) - moq play: a tune-in burst larger than the video queue parks the decoder, so the clock never reaches live at a wide `--delay`
- [Play audio rendition gap](/quest/m2/play-audio-rendition-gap.md) - moq play: a retired audio rendition drains its sink before the replacement fills one, so the switch costs a `--delay` of silence
- [JavaScript FETCH](/quest/m2/js-fetch.md) - generic on-demand group serving and IETF FETCH for browser publishers
- [Archive](/quest/m2/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS, on the catalog and store the release ships
- [Wildcard](/quest/m2/wildcard/README.md) - a relay resolves subscriptions against advertised prefixes, a service claims the prefix it could serve and refuses the rest instead of enumerating broadcasts, and the browser player treats a covering claim as availability
- [#2152](/quest/m2/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - libmoq accepts sessions, the moq-ffi call C still lacks
- [js/publish discontinuity](/quest/m2/js-publish-discontinuity.md) - the JS container producer and js/publish emit the same marker group on encoder restart
- [Failure artifacts](/quest/m2/qa-failure-artifacts.md) - a failing harness run keeps its run directory and a Playwright trace, and CI uploads them
- [Listening kind field](/quest/m2/harness-drive-bys.md) - revert the relay's `kind` field on the listening log lines; nothing reads it
- [Impaired path](/quest/m2/transport-impairment-profile.md) - the transport drills run over a seeded, impaired UDP path on any host
- [Tooling](/quest/m2/tooling/README.md) - justfiles become a one-line menu over `sh/`, one impact map scopes CI, and every workflow step runs a recipe
- [Generation](/quest/m2/hls-generation.md) - init URLs follow the rendition config and segment URLs carry an embedder-supplied generation, so caching can be re-enabled
- [Path patterns](/quest/m2/path-patterns/README.md) - one matcher for every predicate over broadcast paths: tokens, origins, interest
- [In-band auth](/quest/m2/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Stats retier](/quest/m2/stats-retier.md) - a re-checked tier retags a live session's stats in place instead of waiting for its next connection
- [io_uring link facts](/quest/m2/uring-link-facts.md) - the io_uring workers report a session's peer address and SNI to the auth server like the tokio listener does
- [Decoded frame ownership](/quest/m2/decoded-frames.md) - retain moq-video Frames across bindings, with native views or CPU conversion as needed
- [C++ through moq-ffi](/quest/m2/cpp/README.md) - generated C++ over moq-ffi with futures and expected-style errors, shipped as a tarball, vcpkg, and Conan, and adopted by the OBS plugin
- [C# through moq-ffi](/quest/m2/cs/README.md) - generated C# over moq-ffi as a NuGet package with native runtimes
- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - remove FFmpeg decoding dependencies, deliver GPU frames, and use native audio/video encoders
- [Audio codecs](/quest/m2/audio-codecs/README.md) - platform audio codecs, explicit unsupported cases, and channel layouts up to 7.1
- [Opus descriptions](/quest/m2/audio-opus-input.md) - validate headers and honor codec clock, pre-skip, and gain
- [Capture formats](/quest/m2/audio-capture-format.md) - unsupported overrides refuse before device open and channel counts cannot wrap
- [NVENC recovery](/quest/m2/nvenc-recovery.md) - partial initialization and rejected rate changes preserve valid state
- [Transcode source](/quest/m2/transcode-source.md) - select a rendition the chosen backend can actually decode
- [Keyframe trigger](/quest/m2/keyframe-trigger.md) - an application can ask the built-in capture encoder for a keyframe
- [QoS](/quest/m2/qos/README.md) - broadcast health: relay starvation and timeliness histograms, and client stats broadcasts from publishers and viewers
- [Drain](/quest/m2/drain/README.md) - relay restarts drain sessions over GOAWAY instead of hard-dropping them
- [Transport upgrade](/quest/m2/transport-upgrade/README.md) - a session that came up over WebSocket moves to QUIC once the QUIC dial lands, handing over at a group boundary
- [Own the QUIC stack](/quest/m2/quic/README.md) - the moq-noq fork carries
  ACK progress, reliable reset, hierarchical scheduling, deadlines, probing,
  keep-alive, peer limits, careful resume, ECN, and qmux
- [P2P](/quest/m2/p2p/README.md) - opted-in clients serve each other over data channels and iroh while the relay stays the rendezvous and the fallback, under application policy
- [One port](/quest/m2/one-port/README.md) - a relay speaks QUIC, STUN, WebRTC media, and SRT on one UDP port and HTTP, RTMP, and RTMPS on one TCP port
- [Scope track priority](/quest/m2/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [Stream sessions](/quest/m2/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [IETF on the ring](/quest/m2/uring-ietf.md) - the io_uring workers serve moq-transport sessions too, so a uring relay drops no client protocol
- [Perf](/quest/m2/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [#2924](/quest/m2/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - every QUIC worker shares one reloadable served identity, so rotation is atomic and generate works with workers
- [#2964](/quest/m2/2964-quic-workers-dropping-one-split-server-resizes-the.md) - integrate the M1 worker owner with hardened socket-group formation
- [Safari WebTransport](/quest/m2/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - a playout latency regression fails a run instead of arriving as a bug report
- [Latency ledger](/quest/m2/latency-ledger.md) - a session reports where its end-to-end audio delay went, stage by stage
- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - retained evidence, repeated paired runs, and uncertainty for performance claims
- [Audio buffers](/quest/m2/audio-buffers.md) - measure and reduce packetization movement and decoding allocations
- [NVENC reuse](/quest/m2/nvenc-reuse.md) - reuse completed codec resources without weakening ownership
- [Transcode resources](/quest/m2/transcode-resources.md) - measure aggregate threads, codec sessions, retained frames, and probe costs
- [Renderer resources](/quest/m2/video-render-resources.md) - bound surface retention and validate output resources
- [#3126](/quest/m2/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench reports per-interval latency percentiles so the ramp leaves the steady state
- [Relay profiling](/quest/m2/performance-profiles.md) - reproducible CPU and allocation captures under the existing workloads
- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - measure JS transport, container, decode, and render costs in an identified browser
- [JS publish and watch hot paths](/quest/m2/js-hotpath/README.md) - measure decode retention, copies, header writes, and bounded audio groups
- [Reader buffering](/quest/m2/stream-buffering.md) - measure and bound repeated prefix copying under fragmented input
- [Traffic counter contention](/quest/m2/stats-contention.md) - quantify shared atomic accounting costs across fanout workers
- [Closure counters](/quest/m2/closure-counters.md) - a departed node's return never regresses the closure counters a consumer already saw
- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - establish and reduce the browser container copy budget without changing ownership
- [RTMP interleaving](/quest/m2/rtmp-interleaving.md) - isolate partial messages before optimizing assembly copies
- [Mux and gateway copy budgets](/quest/m2/mux-copies/README.md) - measure and reduce payload copies in import, export, and live HLS
- [Consume JSON snapshot patches](/quest/m2/json-merge.md) - measure consuming patches in snapshot encoding and decoding
- [Relay memory](/quest/m2/relay-memory.md) - remeasure what an announcement costs after prefix routes
- [Origin lookup CPU](/quest/m2/origin-cpu/README.md) - announce and subscribe stay cheap as the live advertisement set grows
- [Routing cost domains](/quest/m2/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
- [PoP skipping](/quest/m2/pop-skipping/README.md) - short cold paths for unpopular broadcasts without losing warm backhaul dedup
- [Route cost in the JS origin](/quest/m2/route-cost.md) - the browser origin ranks routes by cost and hops like Rust instead of newest-first
- [Publish channel count](/quest/m2/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence
- [JS abandonment](/quest/m2/js-subscribe-abandonment.md) - a viewer returning during IETF subscribe setup keeps its track across microtasks
- [IETF stream types](/quest/m2/ietf-uni-stream-types.md) - padding streams are discarded stream-only and an unknown uni type closes the session, per draft-21
- [Control timeout code](/quest/m2/control-timeout-code.md) - an unanswered control request resets with CONTROL_TIMEOUT on lite and INTERNAL_ERROR on IETF
- [#2991](/quest/m2/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - one dynamic producer per track name in both languages, with the sequence namespace surviving a replacement
- [JS track end](/quest/m2/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - js/net declares a track end ahead of the live edge and observes the publisher's SUBSCRIBE_END
- [E2EE](/quest/m2/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Processor](/quest/m2/processor/README.md) - a customer-run worker publishes an on-demand contribution with scoped access
- [#3056](/quest/m2/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: the video decoder resets on a declared discontinuity
- [#933](/quest/m2/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the catalog rotation follows the live camera's orientation
- [#2075](/quest/m2/2075-mirror-catalog-reservation-gating-in-moq-hang-js-hang.md) - @moq/publish gates the first catalog snapshot until every reserved track is described
- [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [Ladder](/quest/m2/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [LOC duration marker](/quest/m2/loc-duration-marker.md) - LOC producers write the marker once released consumers skip it
- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - hang: expose the fixed catalog-root broadcast clock without synchronizing library playback to wall time
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [Metadata association](/quest/m2/metadata-association.md) - settle timing and grouping for ID3, SCTE-35, emsg, and FLV independently of SEI separation
- [#2279](/quest/m2/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [Caption import](/quest/m2/captions-import.md) - fMP4 and MKV subtitle tracks import as text renditions instead of erroring or being dropped
- [MSF caption roles](/quest/m2/captions-msf.md) - an MSF caption, subtitle, or sign-language track survives conversion to a hang catalog
- [CEA-608/708](/quest/m2/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Colour model](/quest/m2/color-model.md) - the catalog describes a rendition's colour and HDR properties instead of leaving a TODO
- [#2067](/quest/m2/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - Open-GOP H.264: a regression fixture and a measured cold tune-in
- [Open-GOP leading pictures](/quest/m2/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - video with periodic intra refresh publishes, imports, and tunes in cleanly with one group per sweep and a catalog `warmup`
- [Audio warmup](/quest/m2/audio-warmup.md) - a viewer joining an Opus rendition mid-stream never hears the unconverged first 80 ms
- [#3021](/quest/m2/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - GStreamer maps every pad onto one continuous broadcast clock across source restarts
- [Mux rate](/quest/m2/ts-mux-rate.md) - moq ts: the source multiplex rate is recorded at import and export pads to it, so a CBR stream leaves as one
- [#2779](/quest/m2/2779-moq-export-ts-continuity-counters-are-numbered-from.md) - moq export ts: continuity counters are numbered from process state, so two exporters of the same broadcast emit streams that can never be compared
- [#2829](/quest/m2/2829-moq-export-ts-the-audio-video-interleave-is-decided-by.md) - moq export ts: the audio/video interleave is decided by arrival timing, so two exporters of one broadcast render the same media in different orders
- [#3489](/quest/m2/3489-ts-import-stream-liveness.md) - moq import ts: every elementary stream reports its access units and how long it has been quiet
- [SRT import stats](/quest/m2/srt-import-stats.md) - the SRT gateway reports the same per-stream counters instead of nothing
- [Text availability](/quest/m2/text-schema.md) - a text track publishes its own coverage index instead of copying the media timeline
- [SRT metadata parity](/quest/m2/srt-metadata.md) - the SRT publisher preserves MPEG-TS metadata byte-faithfully like the CLI importer
- [ID3 catalog section](/quest/m2/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [fMP4 emsg](/quest/m2/emsg.md) - event messages survive fMP4 import instead of being silently discarded
- [FLV script tags](/quest/m2/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [Release size](/quest/m2/release-size.md) - the release scripts' LTO exports become the workspace release profile, and a nightly report shows what each moq-ffi build ships
- [Bindgen CLI split](/quest/m2/uniffi-cli-feature.md) - a library build of moq-ffi stops compiling uniffi_bindgen and its 46 crates
- [Network-only bindings](/quest/m2/slim-bindings/README.md) - Swift, Kotlin, and C ship a codec-free `net` artifact beside the full one, if the post-LTO numbers justify it
- [Mobile bindings](/quest/m2/mobile/README.md) - preserve the FFI video consumer and Dart device proof while mobile capture is deferred
- [Compressed tracks](/quest/m2/flate/README.md) - any track compresses per group from every language, not only the JSON modes
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - DMA-BUF encode, H.265 decode, and pre-generated bindings that remove the libclang build dependency, all gated on a moq-dev/vaapi release
- [libmoq shutdown](/quest/m2/libmoq-shutdown.md) - OBS exits cleanly with the plugin loaded: a C ABI `moq_shutdown` stops the libmoq thread before the module is unloaded
- [Kotlin JVM exit](/quest/m2/kt-jvm-exit.md) - a Kotlin/JVM program exits cleanly whatever the moq-ffi runtime thread is doing, like Python does since #3766
- [Dart leaks](/quest/m2/dart-leak.md) - the generated Dart bindings leak native memory on every call
- [Dart publish](/quest/m2/dart-publish.md) - the packages are built and dry-run clean but exist nowhere consumers can install from
- [Dart codec parity](/quest/m2/dart-codecs.md) - Dart is the one binding that cannot originate media
- [libmoq fetch](/quest/m2/libmoq-fetch.md) - libmoq gains an additive cached-group fetch entry point
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [#2147](/quest/m2/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - moq-video: 10-bit HEVC and AV1 support in the NVIDIA codec path
- [#2907](/quest/m2/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - Bind the browser through moq-ffi/UniFFI instead of a second hand-written wasm API
- [#2850](/quest/m2/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - js/net: decode messages synchronously from buffered bytes and delete the publisher read-ahead queue (dev)
- [Cluster flags](/quest/m2/cluster-flags.md) - a discovery mechanism carries its own prerequisites, so an incomplete cluster config cannot be expressed
- [Install moq](/quest/m2/moq-installer.md) - one command installs or upgrades the released CLI on macOS and Linux
- [Install URL](/quest/m2/moq-install-url.md) - moq.dev serves the canonical installer at /install.sh
- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - the relay runs under a `moq` verb with its own flags and TOML, while `moq-relay` stays a minimal binary
- [`moq` serves like a relay](/quest/m2/cli-serve.md) - a `moq --listen` session is authenticated, scoped, counted, and drained like a relay's; the relay is `moq` listening by default
- [#3137](/quest/m2/3137-moqsrc-bound-the-pending-rendition-subscriptions-a.md) - moqsrc: bound the pending rendition subscriptions a catalog can open
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [#709](/quest/m2/709-automatic-letsencrypt-support.md) - the relay provisions and renews its own ACME certificate over HTTP-01, persisted on disk
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - run exact source snapshots on accessible Linux and device hosts with retrievable debug evidence
- [Media QA on other engines](/quest/m2/browser-media-qa-engines.md) - the media harness measures a Firefox or WebKit player over the fallback and names what each engine lacks
- [#1310](/quest/m2/1310-why-use-the-worklet-plugin.md) - why use the worklet plugin?
- [Capture without V4L2 bindgen](/quest/m2/capture-v4l-bindings.md) - moq-video capture builds on Linux without libclang or kernel headers
- [Audio capture without ALSA link](/quest/m2/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound
- [Ship capture and playback](/quest/m2/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [Windows capture parity](/quest/m2/capture-windows.md) - system audio and screen cursor capture with a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture with explicit display-selection and app-capture limits
- [Plan capture ergonomics](/quest/m2/capture-ergonomics.md) - scope independent crop and audio mixing quests
- [X11 capture transport](/quest/m2/x11-capture-shm.md) - move X11 capture to shared memory and RandR events instead of a per-frame socket copy
- [io_uring flow control](/quest/m2/uring-flow-control-windows.md) - the relay's io_uring workers honor the QUIC flow-control windows instead of refusing them
- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - stop rebuilding a full-frame buffer every tick in the X11 and Windows backends
