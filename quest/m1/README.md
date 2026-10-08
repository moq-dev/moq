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

A line with no named consumer waits in m3 until one appears; the 2026-10-05
audit moved P2P, ladder, processor, and timed metadata there from m2.

Decided in the 2026-10-05 audit: the clock chain ranks first (the untimed models,
CMAF frame timestamp, the shared clock, and the two publish-timestamp
quests), with FFI shape pulled up inside it because publishing never invents
a timestamp requires its JSON pilot. The hard fork follows, ahead of perf, and the rest
of the QUIC line follows the fork. Every blocker ranks above the work it
blocks. The quests that gated m0 lines moved under them.

## Required

- [One max_age meaning](/quest/m1/cache-max-age.md) - a superseded group goes stale on wall clock since its successor arrived or on media time, whichever is first, in Rust and js/net; fixes the untimed failover stall
- [Untimed decisions](/quest/m1/untimed-decisions.md) - the maintainer decides whether moq-archive keeps refusing untimed tracks, and whether a malformed FETCH object ends its track
- [Untimed by default in Rust](/quest/m1/rust-untimed-default.md) - an undeclared Rust timescale means untimed, and shared-clock publishers declare milliseconds
- [CMAF frame timestamp](/quest/m1/cmaf-frame-timestamp.md) - CMAF decoders time samples from the moq-lite frame timestamp, using `tfdt` only within the fragment
- [CMAF sample defaults](/quest/m1/cmaf-sample-defaults.md) - one trun, tfhd, trex resolver and one keyframe rule in the importer, the Rust decoder, and JS
- [Shared import clock](/quest/m1/shared-clock.md) - an importer joining a clock already in use offsets its PTS instead of moving it, so captures and imports share one timeline
- [FFI shape](/quest/m1/ffi-shape/README.md) - the bindings mirror Rust's layers: net at the root, then media, json, flate, audio, and video namespaces built from the handle below
- [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md) - no Rust or binding publish API fills in a timestamp; an untimed payload goes out untimed
- [JS publishing never invents a timestamp](/quest/m1/js-publish-timestamp.md) - the same in @moq/json, @moq/flate, and @moq/net
- [Hard fork](/quest/m1/quic/fork/README.md) - quinn hard-forked in-tree as `moq-quic`, ranked ahead of perf; the rest of the QUIC line follows it
- [Cluster routing](/quest/m1/cluster-routing/README.md) - any node routes toward a broadcast's origin over CDN and P2P links alike, with per-origin routes and path-less announces
- [One route cost](/quest/m1/route-cost.md) - Warm and Cold collapse to one static route cost
- [moq-transport deviations](/quest/m1/ietf-deviations-doc.md) - `standard.md` lists the deviations Fastly's interop report flagged (one publisher per path, dropped object properties, OK before an old lite source answers) with their reasons
- [Flat questlines](/quest/m1/quest-flat-lines.md) - moq pins the current quest CLI, lands its questline branches on main, and retires them
- [moq-bot may push workflow changes](/quest/m1/bot-workflows-permission.md) - condition: the maintainer grants moq-bot's GitHub App the `workflows` permission, so back-merges carrying workflow changes go through
- [main merges through a squash queue](/quest/m1/merge-queue-settings.md) - condition: once `release` carries the new back-merge script, the maintainer enables the squash merge queue and moq-bot's pull_request bypass together
- [Binding audio delay](/quest/m1/binding-surface.md) - moq-ffi and every wrapper configure and observe audio playout delay
- [fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md) - fMP4 export writes avc3/hev1 entries from the catalog, so an Annex-B H.264 or H.265 init no longer waits for the first keyframe
- [Bump web-transport-iroh for the capsule close](/quest/m1/iroh-capsule-bump.md) - condition: moq-dev/web-transport#419 ships in a release, then moq's iroh HTTP/3 client reports a peer's close capsule
- [Early video demand](/quest/m1/catalog-early-demand.md) - a regression guards demand before the first keyframe after the detector removal
- [Hang changelog](/quest/m1/hang-changelog-04.md) - the hang draft lists under -03 only what -03 published
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - every stream group in a lite subscription arrives or is dropped by name, and lite-07 drops its stream count for it
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - one dynamic producer per track name in both languages, with the sequence namespace surviving a replacement
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges in either order and replaces FETCH; relays fill misses by range, including over moq-transport
- [Cold relay Largest](/quest/m1/ietf-cold-largest.md) - a relay reports the larger of its upstream's Largest and the largest object it received, even with nothing cached, so a d14-19 joining FETCH through it gets the current group's head
- [Live media time](/quest/m1/subscribe-live-time.md) - re-scoped against `set_live`: a lite-07 SUBSCRIBE_OK carries the publisher's current media time only if a reader still needs it
- [Cross-relay bursts re-run](/quest/m1/cross-relay-bursts.md) - condition: the #4349 reporter re-runs their A/B/C comparison against current cdn.moq.pro
- [Cross-relay FETCH over moq-transport](/quest/m1/ietf-peer-fetch-old.md) - a moq-transport peer link serves held groups instead of refusing them as old, and the drill covers it
- [Untimed lite-07](/quest/m1/lite-untimed.md) - lite-07 carries an untimed track in both languages; lite-05/06 write send time
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [lite-07 Live flag](/quest/m1/lite-live.md) - a separate `Live` field on lite-07 SUBSCRIBE, so merged floors never starve a subscriber; late lower groups build on it
- [Late lower groups](/quest/m1/lite-late-lower-group.md) - a moq-lite subscriber with a floor receives a group created below the first served one, as moq-transport does
- [Flate stream budget](/quest/m1/flate-stream-budget.md) - a flate stream refuses an oversized append without ending, sharing one DEFLATE bound with json
- [JS track takeover](/quest/m1/js-track-takeover.md) - JS `createTrack` answers a queued request and continues its sequences, as Rust does
- [JS query lifetime](/quest/m1/js-probe-lifetime.md) - a pending JS track-info query ends with its last requester, so broadcast demand drops
- [A watch and publish release ships assets()](/quest/m1/assets-release.md) - the release that lets the sites host the worklets
- [Dogfood hosted worklets](/quest/m1/dogfood-assets.md) - moq.dev and the moq.pro dashboard host the worklets and call `assets()` after the release
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [moq-uring tests under load](/quest/m1/uring-tests-under-load.md) - uring tests pass while parallel checks share locked memory
- [Dropped uring session closes](/quest/m1/uring-drop-close.md) - a moq-uring session dropped without close() closes its connection
- [Media audio-tone check](/quest/m1/media-audio-tone.md) - the media lane's audio-tone check passes under load, fixed at its cause
- [CI runner stalls](/quest/m1/ci-runner-stalls.md) - the 0.4 to 0.8 s freezes of both interop tracks on CI are attributed from a week of nightlies and fixed or told apart from playback bugs
- [Browser interop cells](/quest/m1/interop-browser-timeouts.md) - `go -> js` and `python -> js` pass, fixed at the cause rather than by a longer timeout
- [Wire compatibility](/quest/m1/wire-compat.md) - a nightly run tests this checkout against the last published release for tokens, session wire, and catalog/container
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - importers sharing one `--epoch` and fed one stream publish identical groups and timestamps, so failover between a redundant pair survives
- [cpal loads libasound at runtime](/quest/m1/cpal-alsa-runtime.md) - condition: a cpal release whose Linux build carries no load-time libasound requirement
- [Audio capture without ALSA link](/quest/m1/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound
- [Capture by default](/quest/m1/capture-default.md) - moq-video and moq-audio build `capture` by default, so pre-merge checks test it and the capture gate goes away
- [Ship capture and playback](/quest/m1/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [moq.sh deploys from CI](/quest/m1/moq-sh-deploy.md) - the first `release` run of the moq.sh workflow deploys with the Workers Editor token
- [Plan: untimed verbatim PES](/quest/m1/plan-ts-pes-untimed.md) - decide how a verbatim TS track carries a PES that has no PTS, then write the implementation quest
- [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) - json and binary consumers return each value's timestamp, in Rust and every binding; snapshots add `latest()` beside an in-order `next()`
- [JS data consumer timestamps](/quest/m1/js-data-consumer-timestamps.md) - @moq/json and @moq/flate consumers return each value's timestamp, with snapshot `next()` and `latest()`
- [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md) - a draft-20+ FETCH within one group is served from its LOCATION_FILTER, as older drafts are
- [Pipelined first FETCH](/quest/m1/pipeline-fetch-info.md) - a fetch-only reader's first FETCH goes out with the track-info request, on lite and moq-transport
- [FETCH_OK properties](/quest/m1/fetch-ok-properties.md) - our FETCH_OK carries the track properties SUBSCRIBE_OK does, as draft 16+ requires
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - a relay fetches from an IETF upstream without subscribing, finished tracks included, with End of Track always reported
- [IETF object gaps](/quest/m1/ietf-object-gaps.md) - a gapped object ID is refused loudly like a subgroup in Rust and JS, and an overflowing one closes the session in Rust
- [JS IETF datagrams](/quest/m1/js-ietf-datagram.md) - `@moq/net` sends and receives datagram groups over moq-transport, like Rust
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [JS request window](/quest/m1/js-request-window.md) - @moq/net grants MAX_REQUEST_ID as requests close, so a peer never stalls after ~21k requests
- [Relay session limits](/quest/m1/relay-session-limits.md) - moq-relay sets per-session request limits, tighter for clients than peers, and the bindings name a refused request
- [Churn with held subscriptions](/quest/m1/session-churn-held.md) - opening and closing a request costs the same with 1 or 1,024 held subscriptions
- [Wide varint tests](/quest/m1/varint-test-gaps.md) - wide sequences and both varint forms are tested where they differ
- [Export linger](/quest/m1/export-ts-linger.md) - `moq export ts --linger` fails on an export error while the broadcast is live, and lingers once it closes or is replaced
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; the catalog entry and format may break in place, since no archives exist
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Dropped sources](/quest/m1/dropped-sources.md) - track consumers see the producer's real error on every end path, never `Dropped`
- [C++ through moq-ffi](/quest/m1/cpp/README.md) - generated C++ over moq-ffi with futures and expected-style errors, shipped as a tarball, vcpkg, and Conan, and adopted by the OBS plugin
- [OBS publishes under epochs](/quest/m1/obs-epoch.md) - each OBS Start Streaming is a fresh epoch, through the generated C++
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [The final libmoq release is the stub](/quest/m1/libmoq-final-release.md) - the release that lets `rs/libmoq` go
- [Retire the libmoq stub](/quest/m1/libmoq-retire.md) - the published `libmoq` crate stops after its final release points users at `moq-c`
- [OBS native codecs](/quest/m1/obs-moq-video/README.md) - replace FFmpeg video and audio decoding with moq-video and moq-audio, deliver GPU frames, and use native audio/video encoders
- [AudioToolbox decode](/quest/m1/audio-decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [HE-AAC catalog output](/quest/m1/he-aac-catalog-output.md) - HE-AAC catalog entries name the output rate and layout, not the LC core
- [AudioToolbox encode](/quest/m1/audio-encode-audiotoolbox.md) - macOS and iOS encode AAC-LC
- [FFI frame duration default](/quest/m1/ffi-frame-duration-default.md) - the binding audio encoder takes the codec's own frame by default, so `aac()` needs no explicit 0
- [mp4-atom dOps mapping](/quest/m1/mp4-atom-dops-mapping.md) - a released mp4-atom reads and writes any `dOps` channel mapping family and table
- [CMAF surround Opus](/quest/m1/cmaf-opus-surround.md) - fMP4 import and export carry an Opus channel mapping table
- [A self-hosted NVIDIA runner is registered](/quest/m1/gpu-runner.md) - the maintainer registers the host that runs the NVIDIA tests
- [GPU CI](/quest/m1/gpu-ci.md) - NVIDIA tests run nightly on a self-hosted GPU runner, and `just rs nvidia` runs them locally instead of skipping
- [NVENC keyframe flag](/quest/m1/nvenc-keyframe-flag.md) - NVENC flags keyframes from its reported picture type instead of scanning the bitstream
- [Rendition preference](/quest/m1/rendition-preference.md) - automatic selection by `<moq-watch>`, `Video::ranked`, and WHEP keeps the highest `preference` that decodes, so a compatibility transcode is only picked when nothing preferred decodes
- [Audio rendition pick](/quest/m1/audio-ranked.md) - single-track FLV/RTMP and WHEP serve the best audio rendition, not the first by name
- [Own the QUIC stack](/quest/m1/quic/README.md) - quinn hard-forked in-tree as `moq-quic`, carrying BBR, reliable reset, hierarchical scheduling, peer limits, and endpoint sharding
- [QoS](/quest/m1/qos/README.md) - broadcast health: relay starvation and timeliness histograms
- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - catalog rendition keys become IDs unique across video and audio, with an optional `track` name, so one catalog lists renditions from several broadcasts
- [Media stats](/quest/m1/stats/README.md) - publishers announce a stats track in the catalog, viewers answer a soliciting catalog through a per-catalog `.echo` broadcast, and one model turns both into a health verdict and a preflight report
- [JS track handover](/quest/m1/js-group-handover.md) - a JS track subscription resumes across a route swap from the first frame it lacks, so `test/drain` passes at zero latency budget
- [JS startup hole](/quest/m1/js-startup-hole.md) - a `@moq/hang` consumer delivers its first live group at once, even when an empty group died before it
- [JS GOAWAY requests](/quest/m1/js-goaway-requests.md) - after GOAWAY the JS client opens no new request on the old session, like Rust
- [Drain handshakes](/quest/m1/drain-handshakes.md) - a drain GOAWAYs and waits for sessions still in their handshake instead of exiting under them
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - a session that came up over WebSocket moves to QUIC once the QUIC dial lands, handing over without dropping a group
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [IETF on the ring](/quest/m1/uring-ietf.md) - the io_uring workers serve moq-transport sessions too, so a uring relay drops no client protocol
- [BBR ACK cleanup](/quest/m1/bbr-ack-cleanup.md) - packet bookkeeping scales with completed entries instead of scanning the flight on every ACK
- [Benchmark comparisons](/quest/m1/performance-comparisons.md) - retained evidence, repeated paired runs, and uncertainty for performance claims
- [Relay profiling](/quest/m1/performance-profiles.md) - reproducible CPU and allocation captures under the existing workloads
- [Perf](/quest/m1/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [#2924](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - every listener on both runtimes shares one reloadable served identity, so rotation is atomic and generate works with workers
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - PRs get a non-blocking comparison of the Criterion benches they affect, and a nightly trend on main alerts on regressions
- [Mergeable bench buckets](/quest/m1/bench-buckets.md) - moq-bench emits per-interval latency buckets that sum across processes and hosts
- [Relay session bench](/quest/m1/bench-relay.md) - the same scenario through moq-relay's own connection handling
- [Browser benchmarks](/quest/m1/browser-benchmarks.md) - measure JS transport, container, decode, and render costs in an identified browser
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - the browser runs moq-net as TypeScript generated from the Rust source, retiring js/net's hand-written protocol and model code
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m1/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Cache expiry growth](/quest/m1/cache-expiry-growth.md) - with the default pool, relay memory plateaus at the expiry window on every version
- [Frame slot charge](/quest/m1/frame-slot-charge.md) - a group's frame slots past the first four count against the cache pool, including capacity a released group keeps
- [Front deadlines](/quest/m1/front-deadline-index.md) - a front's per-event cost stops growing with its track count: an expiry index and per-track wakes, proven by a churn benchmark
- [Listener deadlines](/quest/m1/listener-deadlines.md) - io_uring, HTTP/2, and the internal listener bound slow handshakes and headers, and iroh honors `quic.keep_alive`
- [Papercuts](/quest/m1/papercuts.md) - JS refuses to serve a broadcast it did not produce, and a uring test stops sleeping
- [Rust papercuts](/quest/m1/papercuts-rs.md) - HTTP refusals are counted, a `u64::MAX` resume is unbounded, GOING_AWAY falls forward, and GRO stride 0 cannot panic
- [JS papercuts](/quest/m1/papercuts-js.md) - IETF status 0 keeps the subgroup open, a muted rendition change settles, and bad element attributes warn
- [Front parking](/quest/m1/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [Route wakes](/quest/m1/route-wakes.md) - a route change wakes only the fronts it can move, so pool churn stops scaling with served paths
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence
- [JS request deadline](/quest/m1/js-request-deadline.md) - each JS request, PUBLISH_NAMESPACE included, has one fatal 10 s timer from create to answer and fails fast without stream credit, so nothing queues or retries
- [Rust request credit](/quest/m1/rs-request-credit.md) - a moq-net request fails at once without stream credit instead of waiting
- [qmux no-wait opens](/quest/m1/qmux-no-wait.md) - @moq/qmux rejects an over-limit create when waitUntilAvailable is false, like Chrome
- [E2EE](/quest/m1/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the catalog rotation follows the live camera's orientation
- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - js/watch: the audio ring converges by time-stretching instead of skipping or going silent
- [Native audio quality](/quest/m1/audio-quality-native.md) - the browser lane's profiles, budgets, and metric schema run against `moq play` on a dummy device
- [Caption import](/quest/m1/captions-import.md) - fMP4 and MKV subtitle tracks import as text renditions instead of erroring or being dropped
- [MSF caption roles](/quest/m1/captions-msf.md) - an MSF caption or subtitle track survives conversion to a hang catalog
- [Encoder colour](/quest/m1/color-model.md) - every moq-video encode path signals the colour its output actually has, or refuses instead of mislabelling
- [T-STD TS export](/quest/m1/tstd/README.md) - `moq export ts` is a proper remux that passes the T-STD buffer model, starting with a fixed `--delay`
- [Rust non-continuous signal](/quest/m1/rust-continuous.md) - the Rust container consumer reports a frame after a subscribe or discontinuity as non-continuous, like JS, for the tune-in and warmup trims
- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [Watch decode errors](/quest/m1/watch-decode-error.md) - a WebCodecs error ends the subscription and the element reports it
- [Catalog warmup](/quest/m1/catalog-warmup.md) - `warmup` on video and audio renditions, in the catalog and the draft
- [Audio warmup](/quest/m1/audio-warmup.md) - a viewer joining an Opus rendition mid-stream never hears the unconverged first 80 ms
- [#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - moq-gst picks the broadcast wall epoch; a restarted source is a new epoch, not a forward re-anchor
- [TS passthrough](/quest/m1/ts-passthrough.md) - `--passthrough` carries the multiplex as whole packets, listed in an `m2ts` catalog section, and writes it back byte-identical (less late drops) on a fixed delay
- [FLV export delay](/quest/m1/flv-export-delay.md) - FLV interleaves through the shared fixed-delay release stage
- [MKV export delay](/quest/m1/mkv-export-delay.md) - MKV interleaves through the shared fixed-delay release stage
- [MKV lacing](/quest/m1/mkv-lacing.md) - laced MKV blocks import as one timed frame each, refused without DefaultDuration
- [Release profile](/quest/m1/release-profile.md) - every release build gets fat LTO, one codegen unit, and stripping from the workspace profile instead of three script exports
- [Size report](/quest/m1/size-report.md) - a nightly job reports every shipped artifact's size, native and JS, and alerts when one grows
- [JS bundle trims](/quest/m1/js-bundle-trims.md) - no bowser, split pako, and lazy qmux and captions
- [Slim Docker images](/quest/m1/docker-slim.md) - images carry only the package's nix closure, not ~170 MiB of nixos/nix
- [Bindings size profile](/quest/m1/ffi-size-profile.md) - a benchmark decides whether the moq-ffi builds ship at opt-level "s", which halves the dylib
- [Go mirror delivery](/quest/m1/go-mirror-delivery.md) - the Go binding's staticlibs stop growing git history by ~210 MiB per release
- [Relay iroh opt-in](/quest/m1/relay-iroh-opt-in.md) - moq-relay drops iroh from its defaults and shipped builds, while moq-cli keeps it for P2P
- [Dart on iOS](/quest/m1/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [Kotlin JVM exit](/quest/m1/kt-jvm-exit.md) - a Kotlin/JVM program exits cleanly whatever the moq-ffi runtime thread is doing, like Python does since #3766
- [Dart publish](/quest/m1/dart-publish.md) - a `moq-dart-v*` tag publishes `moq` to pub.dev unattended, as `moq_ffi`'s tags already do
- [Dart codec parity](/quest/m1/dart-codecs.md) - Dart is the one binding that cannot originate media
- [`moq --listen` drain and stats](/quest/m1/cli-serve.md) - a listening CLI session, already admitted through the relay's auth, is counted and drained like a relay's
- [#709](/quest/m1/709-automatic-letsencrypt-support.md) - the relay provisions and renews its own ACME certificate through rustls-acme over TLS-ALPN-01, persisted on disk
- [Draft 14-16 updates](/quest/m1/ietf-legacy-updates.md) - Rust and JS apply and answer moq-transport 14-16 request updates without ending or leaking the request
- [JS session parity](/quest/m1/js-session-parity.md) - @moq/net gets moq-net's per-session caps, pending-tail hold, and resolved epochs
- [Headless subgroup in Rust](/quest/m1/ietf-headless-subgroup.md) - a draft 14-17 subgroup starting mid-group is dropped, not fatal, as in JS
- [In-band CMAF follow-ups](/quest/m1/cmaf-inline-followups.md) - MSF, h264/h265 export, and gst caps handle avc3/hev1 CMAF
- [Datagram replay bound](/quest/m1/datagram-replay-bound.md) - a new Rust datagram subscriber starts within its max delay of the newest datagram, not at a minutes-old buffer
