# m2: features

## Goal

New capabilities on stable surfaces: wire and routing extensions, QoS,
E2EE, sidecar metadata, gateways, bindings, and developer-facing packages.

## Plan

Ordered by expected demand. The questlines migrated from the downstream
moq.pro tree keep their internal priority; their product halves (pins,
dashboards, fleet rollout) stay downstream.

A few entries are defects rather than capabilities: an m0 quest whose blocker
is an unreleased dependency, or whose symptom nobody is hitting, sits next to
the feature it shares code with instead of holding a rank in m0 that nothing
can act on. Each still carries its own plan and regression test.

## Quests

- [Generation](/quest/m2/hls-generation.md) - init URLs follow the rendition config and segment URLs carry an embedder-supplied generation, so caching can be re-enabled
- [Wildcard](/quest/m2/wildcard/README.md) - a service advertises a path pattern it could serve instead of enumerating broadcasts
- [Path patterns](/quest/m2/path-patterns/README.md) - one versioned matcher for every predicate over broadcast paths: tokens, origins, interest
- [In-band auth](/quest/m2/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Auth API](/quest/m2/auth-api/README.md) - the relay's auth endpoint names mTLS peers, scopes them explicitly on v1, moves a tier in place, and says once when revalidation is off
- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - remove FFmpeg decoding dependencies, deliver GPU frames, and use native audio/video encoders
- [Audio codecs](/quest/m2/audio-codecs/README.md) - a broadcast that plays in the browser plays natively: platform decoders and encoders behind a backend seam, HE-AAC, and channel layouts up to 7.1
- [Keyframe trigger](/quest/m2/keyframe-trigger.md) - an application can ask the built-in capture encoder for a keyframe
- [QoS](/quest/m2/qos/README.md) - broadcast health: relay starvation and timeliness histograms, and client stats broadcasts from publishers and viewers
- [Drain](/quest/m2/drain/README.md) - relay restarts drain sessions over GOAWAY instead of hard-dropping them
- [Custom QUIC](/quest/m2/quic/README.md) - noq as the upstream for per-stream
  ACK progress, reliable reset, hierarchical scheduling, and qmux
- [Bandwidth estimate release](/quest/m2/web-transport-bandwidth-estimate.md) - web-transport-quinn reports quinn's BBR bandwidth estimate and ships a release carrying it
- [#2847](/quest/m2/2847-the-quinn-backends-send-bandwidth-estimate-is-cwnd-rtt.md) - quinn backend: bump to the releases that report the controller bandwidth estimate instead of cwnd/rtt
- [Safari WebTransport](/quest/m2/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - a playout latency regression fails a run instead of arriving as a bug report
- [Latency ledger](/quest/m2/latency-ledger.md) - a session reports where its end-to-end audio delay went, stage by stage
- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - retained evidence, repeated paired runs, and uncertainty for performance claims
- [Relay profiling](/quest/m2/performance-profiles.md) - reproducible CPU and allocation captures under the existing workloads
- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - measure JS transport, container, decode, and render costs in an identified browser
- [JS publish and watch hot paths](/quest/m2/js-hotpath/README.md) - measure decode retention, copies, header writes, and bounded audio groups
- [Reader buffering](/quest/m2/stream-buffering.md) - measure and bound repeated prefix copying under fragmented input
- [Traffic counter contention](/quest/m2/stats-contention.md) - quantify shared atomic accounting costs across fanout workers
- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - establish and reduce the browser container copy budget without changing ownership
- [RTMP interleaving](/quest/m2/rtmp-interleaving.md) - isolate partial messages before optimizing assembly copies
- [Mux and gateway copy budgets](/quest/m2/mux-copies/README.md) - measure and reduce payload copies in import, export, and live HLS
- [Consume JSON snapshot patches](/quest/m2/json-merge.md) - measure consuming patches in snapshot encoding and decoding
- [Relay memory](/quest/m2/relay-memory.md) - remeasure what an announcement costs after prefix routes
- [Origin lookup CPU](/quest/m2/origin-cpu/README.md) - announce and subscribe stay cheap as the live advertisement set grows
- [Route gauge](/quest/m2/route-gauge.md) - an operator sees how many routes a relay holds for a path
- [PoP skipping](/quest/m2/pop-skipping/README.md) - short cold paths for unpopular broadcasts without losing warm backhaul dedup
- [E2EE](/quest/m2/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [SEI](/quest/m2/sei/README.md) - H.26x SEI moves into its own track, readable without subscribing to video
- [Processor](/quest/m2/processor/README.md) - a customer-run worker publishes an on-demand contribution with scoped access
- [Timeline wall](/quest/m2/timeline-wall.md) - every built-in publisher anchors its timeline to wall time; the anchor is data, never a sync source
- [Duration marker](/quest/m2/duration-marker.md) - a video group ends with an empty frame that closes its last frame's duration; audio never writes one
- [LOC duration marker](/quest/m2/loc-duration-marker.md) - LOC producers write the marker once released consumers skip it
- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - hang: a timeline consumer exposes the wall anchor, and the library never syncs playback on it
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [#2279](/quest/m2/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues ride a per-rendition sidecar a player can act on
- [Caption import](/quest/m2/captions-import.md) - fMP4 and MKV subtitle tracks import as text renditions instead of erroring or being dropped
- [MSF caption roles](/quest/m2/captions-msf.md) - an MSF caption, subtitle, or sign-language track survives conversion to a hang catalog
- [CEA-608/708](/quest/m2/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Colour model](/quest/m2/color-model.md) - the catalog describes a rendition's colour and HDR properties instead of leaving a TODO
- [#2067](/quest/m2/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - Open-GOP H.264: a regression fixture and a measured cold tune-in
- [Open-GOP leading pictures](/quest/m2/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [Gradual recovery](/quest/m2/open-gop-gradual-recovery.md) - tune-in at a recovery point with `recovery_frame_cnt > 0` trims through the recovery picture
- [#3021](/quest/m2/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - moq-gst: anchor generated media timelines to wall clock
- [#2779](/quest/m2/2779-moq-export-ts-continuity-counters-are-numbered-from.md) - moq export ts: continuity counters are numbered from process state, so two exporters of the same broadcast emit streams that can never be compared
- [#2829](/quest/m2/2829-moq-export-ts-the-audio-video-interleave-is-decided-by.md) - moq export ts: the audio/video interleave is decided by arrival timing, so two exporters of one broadcast render the same media in different orders
- [#3489](/quest/m2/3489-ts-import-stream-liveness.md) - moq import ts: every elementary stream reports its access units and how long it has been quiet
- [SRT import stats](/quest/m2/srt-import-stats.md) - the SRT gateway reports the same per-stream counters instead of nothing
- [Text availability](/quest/m2/text-schema.md) - a text track publishes its own coverage index instead of copying the media timeline
- [SRT metadata parity](/quest/m2/srt-metadata.md) - the SRT publisher preserves MPEG-TS metadata byte-faithfully like the CLI importer
- [ID3 catalog section](/quest/m2/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [fMP4 emsg](/quest/m2/emsg.md) - event messages survive fMP4 import instead of being silently discarded
- [AV1 metadata OBUs](/quest/m2/av1-metadata.md) - HDR10+, timecode and scalability OBUs become addressable
- [FLV script tags](/quest/m2/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [Mobile](/quest/m2/mobile/README.md) - MoQ runs natively on iOS and Android: an FFI video consumer, the ownership decision, capture, and the Dart device proof
- [Compressed tracks](/quest/m2/flate/README.md) - any track compresses per group from every language, not only the JSON modes
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - DMA-BUF encode, H.265 decode, and pre-generated bindings that remove the libclang build dependency, all gated on a moq-dev/vaapi release
- [Dart leaks](/quest/m2/dart-leak.md) - the generated Dart bindings leak native memory on every call
- [Dart publish](/quest/m2/dart-publish.md) - the packages are built and dry-run clean but exist nowhere consumers can install from
- [Dart codec parity](/quest/m2/dart-codecs.md) - Dart is the one binding that cannot originate media
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [#2147](/quest/m2/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - moq-video: 10-bit HEVC and AV1 support in the NVIDIA codec path
- [#2907](/quest/m2/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - Bind the browser through moq-ffi/UniFFI instead of a second hand-written wasm API
- [#2850](/quest/m2/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - js/net: decode messages synchronously from buffered bytes and delete the publisher read-ahead queue (dev)
- [Cluster flags](/quest/m2/cluster-flags.md) - a discovery mechanism carries its own prerequisites, so an incomplete cluster config cannot be expressed
- [#3137](/quest/m2/3137-moqsrc-bound-the-pending-rendition-subscriptions-a.md) - moqsrc: bound the pending rendition subscriptions a catalog can open
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [#709](/quest/m2/709-automatic-letsencrypt-support.md) - the relay provisions and renews its own ACME certificate over HTTP-01, persisted on disk
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - run exact source snapshots on accessible Linux and device hosts with retrievable debug evidence
- [Media QA on other engines](/quest/m2/browser-media-qa-engines.md) - the media harness measures a Firefox or WebKit player over the fallback and names what each engine lacks
- [LiveKit shim](/quest/m2/livekit-shim.md) - a drop-in livekit-client-compatible package running rooms over MoQ
- [#1310](/quest/m2/1310-why-use-the-worklet-plugin.md) - why use the worklet plugin?
- [Ship capture and playback](/quest/m2/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [Windows capture parity](/quest/m2/capture-windows.md) - window, app, system-audio and cursor capture on Windows
- [Linux capture parity](/quest/m2/capture-linux.md) - window capture, system audio, and a chosen display through the portal
- [Capture ergonomics](/quest/m2/capture-ergonomics.md) - region capture, audio mixing, and validating format overrides
- [X11 capture transport](/quest/m2/x11-capture-shm.md) - move X11 capture to shared memory and RandR events instead of a per-frame socket copy
- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - stop rebuilding a full-frame buffer every tick in the X11 and Windows backends
