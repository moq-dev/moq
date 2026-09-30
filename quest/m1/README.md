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

A line with no named consumer waits in m2 until one appears. The 2026-09-30
audit moved P2P, one-port, ladder, processor, timed metadata, the installer,
the OBS GPU paths, the IETF half of rs2ts, and the unmeasured io_uring and
QUIC studies there on that rule.

## Required

- [Cluster idle timeout](/quest/m1/cluster-idle-timeout.md) - a relay notices a silent peer relay within seconds, not after the shared 30 s QUIC idle timeout
- [Cluster routing](/quest/m1/cluster-routing/README.md) - a relay learns the relay graph once and each announcement once, not once per neighbour, and redundant publishers share an epoch instead of `--hop`
- [Track tail interop](/quest/m1/track-tail-interop.md) - a Rust publisher ending a track with a group in flight is read to its end by the JS subscriber, and the reverse, in `just test interop`
- [Merge queue](/quest/m1/merge-queue.md) - the required checks run on `merge_group`, so a stale green check can no longer break main
- [Binding audio delay](/quest/m1/binding-surface.md) - moq-ffi and every wrapper configure and observe audio playout delay
- [FFI shape](/quest/m1/ffi-shape/README.md) - the bindings mirror Rust's layers: net at the root, then media, json, flate, audio, and video namespaces built from the handle below
- [Android logcat](/quest/m1/android-logcat.md) - on dev, Android builds always log to logcat and the `android-logcat` feature is gone
- [Raw stream codes](/quest/m1/raw-stream-codes.md) - raw QUIC stream resets and stops carry the application's code, not an HTTP/3-mapped one
- [IETF drain before close](/quest/m1/ietf-drain-before-close.md) - moq-transport sessions deliver finished tracks before a graceful close, as moq-lite does
- [Close waits for the tail](/quest/m1/close-tail.md) - on lite-07, `close()` returns `Ok` only after each subscriber FINs its Subscribe Stream, having read the track to its end
- [Demo serve-hls renditions](/quest/m1/serve-hls-renditions.md) - `just pub serve-hls` serves 720p and 144p instead of two 256-wide copies
- [moq play drain tail](/quest/m1/play-drain-tail.md) - retired renditions and finite tracks play their last 10 ms of audio
- [WebTransport close upstream](/quest/m1/wt-close-upstream.md) - web-transport-moq delivers the close capsule itself, and moq-tokio's `CLOSE_LINGER` is deleted
- [Browser close code](/quest/m1/browser-close-code.md) - a playwright case proves the page reads a relay's close code and reason, on every web-transport backend
- [Resumed groups](/quest/m1/resume-latest.md) - a half-delivered group ends once the new copy is past it, so a group-only reader never parks after a mid-group failover
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - every stream group in a lite subscription arrives or is dropped by name, and lite-07 drops its stream count for it
- [Parked reads wake](/quest/m1/parked-read-wakes.md) - a read parked on an evicted or aborted group wakes and re-judges, for resume successors and plain tracks alike
- [Reader end parity](/quest/m1/reader-end-parity.md) - JS readers see a track's end once the newest group reaches the declared end, as Rust readers do
- [Cross-relay bursts](/quest/m1/cross-relay-bursts.md) - bursty small-group tracks cross two relays without lost groups, unanswered FETCHes, or stalls
- [Request stream cancel](/quest/m1/request-stream-serve.md) - a lite publisher stops resolving a SUBSCRIBE or FETCH once the requester FINs or resets, through one wrapper that owns every request stream's reader
- [Session death parity](/quest/m1/session-death.md) - a local close ends tracks cleanly in both languages, and JS group readers see the session's error on session death
- [Watch and publish under CSP](/quest/m1/csp-assets.md) - blob workers stay the default; strict-CSP apps host the files and set a base URL
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [CI runner stalls](/quest/m1/ci-runner-stalls.md) - the 0.4 to 0.8 s freezes of both interop tracks on CI are attributed from a week of nightlies and fixed or told apart from playback bugs
- [Catalog estimate rate](/quest/m1/catalog-estimate-rate.md) - a rising `jitter`/`delay` estimate republishes the catalog at most once a second, in js/publish and moq-mux
- [Legacy end overshoot](/quest/m1/legacy-end-overshoot.md) - browser playback survives a group that starts inside the previous group's estimated end
- [Wire compatibility](/quest/m1/wire-compat.md) - a nightly run tests this checkout against the last published release for tokens, session wire, and catalog/container
- [TS PSI reassembly](/quest/m1/ts-psi-reassembly.md) - `import ts` reads a PAT or PMT that spans packets or follows a nonzero pointer_field instead of aborting, and one corrupted section costs a repetition and a counted `CRC_error`, not the import
- [TS damaged units](/quest/m1/ts-damaged-units.md) - one malformed PES or access unit is dropped, counted as `damaged`, and resynced at the next keyframe instead of ending the import
- [RTMP interleaving](/quest/m1/rtmp-interleaving.md) - isolate partial messages before optimizing assembly copies
- [TS stats module](/quest/m1/ts-stats-module.md) - on dev, the TS stats types move under `ts::stats` as `Snapshot` and `Stream`, with an owned `track`
- [Same-hop importers](/quest/m1/hop-aligned-import.md) - importers fed one stream publish identical groups and timestamps, so failover between a redundant pair survives
- [Audio capture without ALSA link](/quest/m1/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound
- [Capture by default](/quest/m1/capture-default.md) - moq-video and moq-audio build `capture` by default, so pre-merge checks test it and the capture gate goes away
- [Ship capture and playback](/quest/m1/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [Auth client CA](/quest/m1/relay-auth-client-ca.md) - on dev, `auth::Config::validate` and `init` take the client-CA flag, so no caller can skip the check
- [Data track clock](/quest/m1/data-track-clock.md) - JSON and binary data tracks stamp on the catalog's clock at write time, matching the media's anchored clock
- [Go and Dart doc samples](/quest/m1/doc-samples-go-dart.md) - Go and Dart doc samples compile against their wrappers
- [Data capture in bindings](/quest/m1/data-capture-bindings.md) - moq-ffi and every wrapper pass a data frame's capture time, and the JSON window producer takes one
- [Moxygen compatibility](/quest/m1/moxygen/README.md) - one subgroup per group, whole-group FETCH, and one datagram per group, never a full moxygen pass
- [JS IETF datagrams](/quest/m1/js-ietf-datagram.md) - `@moq/net` sends and receives datagram groups over moq-transport, like Rust
- [Datagrams are live-only](/quest/m1/datagram-unfetchable.md) - no FETCH, replay to a new subscriber, or cache fill ever returns a datagram group
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - one dynamic producer per track name in both languages, with the sequence namespace surviving a replacement
- [JavaScript FETCH](/quest/m1/js-fetch.md) - generic on-demand group serving and IETF FETCH for browser publishers
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; the catalog entry and format may break in place, since no archives exist
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Dropped sources](/quest/m1/dropped-sources.md) - track consumers see the producer's real error on every end path, never `Dropped`
- [C++ through moq-ffi](/quest/m1/cpp/README.md) - generated C++ over moq-ffi with futures and expected-style errors, shipped as a tarball, vcpkg, and Conan, and adopted by the OBS plugin
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [OBS native codecs](/quest/m1/obs-moq-video/README.md) - replace FFmpeg video and audio decoding with moq-video and moq-audio, deliver GPU frames, and use native audio/video encoders
- [AudioToolbox decode](/quest/m1/audio-decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [HE-AAC catalog output](/quest/m1/he-aac-catalog-output.md) - HE-AAC catalog entries name the output rate and layout, not the LC core
- [TS Opus export refusals](/quest/m1/ts-opus-export-refusals.md) - the TS exporter refuses Opus heads its channel code cannot describe instead of mislabeling them
- [TS Opus channel codes](/quest/m1/ts-opus-channel-codes.md) - TS Opus with a channel code of 0x81 or above imports with its real head or is refused, never guessed as stereo
- [AudioToolbox encode](/quest/m1/audio-encode-audiotoolbox.md) - macOS and iOS encode AAC-LC
- [AAC encode refusals](/quest/m1/aac-encode-refusals.md) - on dev, `Config::encode` refuses channel counts it cannot name instead of writing stereo
- [AAC parse truncated SBR](/quest/m1/aac-parse-truncated-sbr.md) - `Config::parse` refuses SBR and PS configs cut off before their core
- [GStreamer surround Opus](/quest/m1/gst-opus-surround.md) - the moq-gst sink publishes 3 to 8 channel Opus with the OpusHead its caps describe
- [TS AAC PCE joins](/quest/m1/ts-aac-pce-join.md) - PCE-described AAC over TS plays after a mid-stream join or resume, and a missing PCE silences only its track
- [FFI frame duration default](/quest/m1/ffi-frame-duration-default.md) - on dev, the binding audio encoder takes the codec's own frame by default, so `aac()` needs no explicit 0
- [Opus mapping family](/quest/m1/opus-mapping-family.md) - on dev, the Opus head config keeps its mapping family only in `mapping`
- [mp4-atom dOps mapping](/quest/m1/mp4-atom-dops-mapping.md) - a released mp4-atom reads and writes any `dOps` channel mapping family and table
- [CMAF surround Opus](/quest/m1/cmaf-opus-surround.md) - fMP4 import and export carry an Opus channel mapping table
- [GPU CI](/quest/m1/gpu-ci.md) - NVIDIA tests run nightly on a self-hosted GPU runner, and `just rs nvidia` runs them locally instead of skipping
- [JS rendition ranking](/quest/m1/js-ranked.md) - `@moq/hang` ranks video renditions like Rust, and `@moq/watch`'s fallback uses it
- [Audio rendition pick](/quest/m1/audio-ranked.md) - single-track FLV/RTMP and WHEP serve the best audio rendition, not the first by name
- [FLV catalog stream](/quest/m1/flv-catalog-stream.md) - on dev, `flv::Export` takes a catalog stream like fmp4, replacing `with_select`
- [io_uring flow control](/quest/m1/uring-flow-control-windows.md) - the relay's io_uring workers honor the QUIC flow-control windows instead of refusing them
- [Own the QUIC stack](/quest/m1/quic/README.md) - the moq-noq fork carries ACK progress, reliable reset, hierarchical scheduling, deadlines, peer limits, and qmux
- [QoS](/quest/m1/qos/README.md) - broadcast health: relay starvation and timeliness histograms, on dev
- [Catalog track alias](/quest/m1/catalog-track-alias.md) - catalog rendition keys become aliases with an optional `track` name, so one catalog lists renditions from several broadcasts
- [Media stats](/quest/m1/stats/README.md) - publishers announce a stats track in the catalog, viewers answer a soliciting catalog through a per-catalog `.echo` broadcast, and a Rust encoder adapts to them
- [Drain](/quest/m1/drain/README.md) - relay restarts drain sessions over GOAWAY instead of hard-dropping them
- [Strict Redirect::resolve](/quest/m1/redirect-resolve.md) - on dev, `Redirect::resolve` can no longer quietly turn a refused redirect into a redial
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - a session that came up over WebSocket moves to QUIC once the QUIC dial lands, handing over at a group boundary
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [IETF on the ring](/quest/m1/uring-ietf.md) - the io_uring workers serve moq-transport sessions too, so a uring relay drops no client protocol
- [BBR ACK cleanup](/quest/m1/bbr-ack-cleanup.md) - packet bookkeeping scales with completed entries instead of scanning the flight on every ACK
- [Perf](/quest/m1/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [#2924](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - every listener on both runtimes shares one reloadable served identity, so rotation is atomic and generate works with workers
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - PRs get a non-blocking comparison of the Criterion benches they affect, and a nightly trend on main alerts on regressions
- [Benchmark comparisons](/quest/m1/performance-comparisons.md) - retained evidence, repeated paired runs, and uncertainty for performance claims
- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - moq-bench reports per-interval latency percentiles so the ramp leaves the steady state
- [Relay session bench](/quest/m1/bench-relay.md) - the same scenario through moq-relay's own connection handling
- [Relay profiling](/quest/m1/performance-profiles.md) - reproducible CPU and allocation captures under the existing workloads
- [Browser benchmarks](/quest/m1/browser-benchmarks.md) - measure JS transport, container, decode, and render costs in an identified browser
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - the browser runs moq-net as TypeScript generated from the Rust source, retiring js/net's hand-written protocol and model code
- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m1/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Cache expiry growth](/quest/m1/cache-expiry-growth.md) - with the default pool, relay memory plateaus at the expiry window on every version
- [Plan: cache age-out](/quest/m1/cache-wall-eviction.md) - a swept benchmark decides whether the track cache ages groups out on wall time without a write
- [Frame slot charge](/quest/m1/frame-slot-charge.md) - a group's frame slots past the first four count against the cache pool, including capacity a released group keeps
- [Front deadlines](/quest/m1/front-deadline-index.md) - a front's per-event cost stops growing with its track count: an expiry index and per-track wakes, proven by a churn benchmark
- [Front parking](/quest/m1/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence
- [JS abandonment](/quest/m1/js-subscribe-abandonment.md) - a viewer returning during IETF subscribe setup keeps its track across microtasks
- [Epoch primitive](/quest/m1/epoch.md) - one `Epoch` type in moq-net and @moq/net, carried as a trailing `@<uuidv7>` path segment, shared by e2ee and broadcast epochs
- [E2EE](/quest/m1/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - each publish of a name gets a fresh `@<uuidv7>` epoch, viewers follow the newest live one at once, and bare names still resolve on every version
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - watch: the video decoder resets on a declared discontinuity
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the catalog rotation follows the live camera's orientation
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Native audio quality](/quest/m1/audio-quality-native.md) - the browser lane's profiles, budgets, and metric schema run against `moq play` on a dummy device
- [Caption import](/quest/m1/captions-import.md) - fMP4 and MKV subtitle tracks import as text renditions instead of erroring or being dropped
- [MSF caption roles](/quest/m1/captions-msf.md) - an MSF caption, subtitle, or sign-language track survives conversion to a hang catalog
- [Encoder colour](/quest/m1/color-model.md) - every moq-video encode path signals the colour its output actually has, or refuses instead of mislabelling
- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [Catalog warmup](/quest/m1/catalog-warmup.md) - `warmup` on video and audio renditions, in the catalog and the draft
- [Audio warmup](/quest/m1/audio-warmup.md) - a viewer joining an Opus rendition mid-stream never hears the unconverged first 80 ms
- [#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - moq-gst picks the broadcast wall epoch; a restarted source is a new epoch, not a forward re-anchor
- [TS byte schedule](/quest/m1/ts-export-byte-schedule.md) - moq export ts places PCRs and padding on the byte grid `mpegts.muxRate` implies, so a receiver can clock off arrival
- [Release profile](/quest/m1/release-profile.md) - every release build gets fat LTO, one codegen unit, and stripping from the workspace profile instead of three script exports
- [Size report](/quest/m1/size-report.md) - a nightly job reports every shipped artifact's size, native and JS, and alerts when one grows
- [Publish lazy file source](/quest/m1/publish-lazy-file.md) - a camera or screen `<moq-publish>` stops downloading mediabunny's ~99 KB gzip
- [JS bundle trims](/quest/m1/js-bundle-trims.md) - minified worklets, no bowser, split pako, and lazy qmux and captions
- [Slim Docker images](/quest/m1/docker-slim.md) - images carry only the package's nix closure, not ~170 MiB of nixos/nix
- [Bindings size profile](/quest/m1/ffi-size-profile.md) - a benchmark decides whether the moq-ffi builds ship at opt-level "s", which halves the dylib
- [Go mirror delivery](/quest/m1/go-mirror-delivery.md) - the Go binding's staticlibs stop growing git history by ~210 MiB per release
- [Relay iroh opt-in](/quest/m1/relay-iroh-opt-in.md) - moq-relay drops iroh from its defaults and shipped builds, while moq-cli keeps it for P2P
- [Rust owns mobile codecs](/quest/m1/mobile-ownership.md) - the settled verdict (Rust codecs, CVPixelBuffer bridge) is written where binding work reads it
- [Dart on iOS](/quest/m1/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [Kotlin JVM exit](/quest/m1/kt-jvm-exit.md) - a Kotlin/JVM program exits cleanly whatever the moq-ffi runtime thread is doing, like Python does since #3766
- [Dart publish](/quest/m1/dart-publish.md) - the packages are built and dry-run clean but exist nowhere consumers can install from
- [Dart codec parity](/quest/m1/dart-codecs.md) - Dart is the one binding that cannot originate media
- [#2850](/quest/m1/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - js/net caps the publisher's subscription controls so a flood cannot grow memory without bound
- [`moq --listen` admission](/quest/m1/cli-serve.md) - a listening CLI session is authenticated, scoped, counted, and drained like a relay's instead of accepting everything
- [#709](/quest/m1/709-automatic-letsencrypt-support.md) - the relay provisions and renews its own ACME certificate through rustls-acme over TLS-ALPN-01, persisted on disk
