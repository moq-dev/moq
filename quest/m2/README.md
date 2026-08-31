# m2: features

## Goal

New capabilities on stable surfaces: wire and routing extensions, QoS,
E2EE, sidecar metadata, gateways, bindings, and developer-facing packages.

## Plan

Ordered by expected demand. The questlines migrated from the downstream
moq.pro tree keep their internal priority; their product halves (pins,
dashboards, fleet rollout) stay downstream.

## Quests

- [Plan: HLS identity](/quest/m2/plan-hls-identity.md) - settle where a cacheable generation ID comes from via /plan-quest
- [Generation](/quest/m2/hls-generation.md) - media URLs carry a generation ID so segment caching can be re-enabled
- [Wildcard](/quest/m2/wildcard/README.md) - a service advertises a path pattern it could serve instead of enumerating broadcasts
- [Path patterns](/quest/m2/path-patterns/README.md) - one versioned matcher for every predicate over broadcast paths: tokens, origins, interest
- [Keyframe trigger](/quest/m2/keyframe-trigger.md) - an application can ask the built-in capture encoder for a keyframe
- [QoS](/quest/m2/qos/README.md) - broadcast health counters: network backlog, publisher stats, viewer feedback
- [Drain](/quest/m2/drain/README.md) - relay restarts drain sessions over GOAWAY instead of hard-dropping them
- [Quiche](/quest/m2/quiche/README.md) - media-aware transport features in the moq-dev/quiche fork and their moq-uring adoption
- [Relay memory](/quest/m2/relay-memory/README.md) - relay memory scales with what it serves, not what the fleet knows
- [PoP skipping](/quest/m2/pop-skipping/README.md) - short cold paths for unpopular broadcasts without losing warm backhaul dedup
- [E2EE](/quest/m2/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [SEI](/quest/m2/sei/README.md) - H.26x SEI becomes a first-class hang sidecar that can be stripped and stitched
- [Processor](/quest/m2/processor/README.md) - a customer-run worker publishes an on-demand contribution with scoped access
- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - watch: absolute wall-clock latency target for synchronized playback across viewers
- [#2279](/quest/m2/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: typed SCTE-35 / ad cue signaling (carried opaquely today, unreadable by players)
- [Plan: captions](/quest/m2/plan-captions.md) - fix the cue format and catalog shape for text tracks via /plan-quest
- [#2280](/quest/m2/2280-hang-caption-and-subtitle-text-tracks.md) - hang: caption and subtitle text tracks
- [#3021](/quest/m2/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - moq-gst: anchor generated media timelines to wall clock
- [#2779](/quest/m2/2779-moq-export-ts-continuity-counters-are-numbered-from.md) - moq export ts: continuity counters are numbered from process state, so two exporters of the same broadcast emit streams that can never be compared
- [#2829](/quest/m2/2829-moq-export-ts-the-audio-video-interleave-is-decided-by.md) - moq export ts: the audio/video interleave is decided by arrival timing, so two exporters of one broadcast render the same media in different orders
- [Text schema](/quest/m2/text-schema.md) - a native hang text contract so transcription shares source PTS
- [SRT metadata parity](/quest/m2/srt-metadata.md) - the SRT publisher preserves MPEG-TS metadata byte-faithfully like the CLI importer
- [ID3 catalog section](/quest/m2/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [#2147](/quest/m2/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - moq-video: 10-bit HEVC and AV1 support in the NVIDIA codec path
- [#3164](/quest/m2/3164-moq-audio-remove-heap-activity-and-blocking-locks-from.md) - moq-audio: remove heap activity and blocking locks from the capture callback
- [#3165](/quest/m2/3165-moq-audio-bound-and-coalesce-playback-driver-commands.md) - moq-audio: bound and coalesce playback driver commands
- [#2907](/quest/m2/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - Bind the browser through moq-ffi/UniFFI instead of a second hand-written wasm API
- [#2822](/quest/m2/2822-moq-wasm-bind-the-datagram-path-append-datagram-recv.md) - moq-wasm: bind the datagram path (append_datagram / recv_datagram)
- [#2835](/quest/m2/2835-moq-wasm-bind-track-dynamic-so-a-browser-publisher-can.md) - moq-wasm: bind track::Dynamic so a browser publisher can serve cache-miss fetches
- [#3193](/quest/m2/3193-expose-a-cancellable-route-watch-api-in-python.md) - Expose a cancellable route watch API in Python
- [#3189](/quest/m2/3189-add-uniffi-defaults-to-caller-constructed-configuration.md) - Add UniFFI defaults to caller-constructed configuration records
- [Plan: CLI arguments](/quest/m2/plan-cli-arguments.md) - decide the client/server flag scheme via /plan-quest
- [#2696](/quest/m2/2696-re-evaluate-the-client-server-split-in-cli-arguments.md) - Re-evaluate the client/server split in CLI arguments
- [#3058](/quest/m2/3058-moq-relay-a-revalidation-re-check-cannot-update-a.md) - moq-relay: a revalidation re-check cannot update a session's tier, and changes its alias only by closing it
- [#3137](/quest/m2/3137-moqsrc-bound-the-pending-rendition-subscriptions-a.md) - moqsrc: bound the pending rendition subscriptions a catalog can open
- [#709](/quest/m2/709-automatic-letsencrypt-support.md) - Automatic LetsEncrypt support
- [Plan: multipath](/quest/m2/plan-multipath.md) - split the bonded-contribution multipath work via /plan-quest
- [#2276](/quest/m2/2276-moq-native-enable-quic-multipath-for-bonded-contribution.md) - moq-native: enable QUIC multipath for bonded contribution (noq already implements it)
- [Room SDK](/quest/m2/room-sdk.md) - a headless room package: a room is a path prefix, no service, no storage
- [LiveKit shim](/quest/m2/livekit-shim.md) - a drop-in livekit-client-compatible package running rooms over MoQ
- [Auth verdict](/quest/m2/auth-verdict.md) - the relay hands an opaque credential to its auth API and is told the grant
- [#1310](/quest/m2/1310-why-use-the-worklet-plugin.md) - why use the worklet plugin?
- [Plan: video backlog](/quest/m2/plan-video-backlog.md) - prune and split the moq-video tracking epic via /plan-quest
- [#1837](/quest/m2/1837-moq-video-remaining-work-codecs-platforms-decode-hw.md) - moq-video: remaining work (codecs, platforms, decode, HW validation)
- [Plan: CLI capture](/quest/m2/plan-cli-capture.md) - split the moq-cli capture and playback backlog via /plan-quest
- [#2272](/quest/m2/2272-moq-cli-remaining-work-for-capture-and-playback-window.md) - moq-cli: remaining work for capture and playback (window/app capture, device enumeration, native player)
