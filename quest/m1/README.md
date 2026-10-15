# m1: clustering stability and the breaking release

## Goal

The next breaking release delivers stable clustered routing and recovery,
switches to moq-quic, and consolidates the planned public API and format
changes. Complete this milestone before cutting main into release.

## Plan

Decided with the maintainer on 2026-10-13:

- Clustering stability is the first goal: an ended broadcast cannot return
  through a stale route; a working backup recovers; matching-epoch handover
  preserves eligible content; a new epoch produces an explicit Restart.
  Catalogs, long-lived readers, and late joiners must obey the same contract.
- Prioritize the QUIC library switch beside routing. Its clock prerequisite
  lands first. Keep its correctness, interoperability, memory, and performance
  evidence; a passing simulator alone does not validate a real transport.
- Finish planned breaking APIs in this release, including public clocks,
  signed priority, binding shape, watch worker, and the QoS lag API. Keep
  prerequisites of those changes here even when a prerequisite is additive.
  New optional capabilities and general optimization work follow in m2.
- Finalized lite-07 is preferred by first-party Rust and JS clients, with
  older-version negotiation fallback. Servers accept it by default. The wip
  identifier is never reused for an incompatible mixed-version rollout.
- Validate the GPU input surface on AMD and Intel before publishing it;
  vendor-neutral capacity and health reporting remains m2.
- Package publication, downstream adoption, and production rollout are
  successive stages, not prerequisites of the cut that makes them possible.
  Their owner is [release execution](/quest/m2/release/README.md).

The route handoff and watch-worker planning quests settle their existing
implementation choices before code is written. The binding namespace spike
must produce a go/no-go before generated package names freeze. Existing
upgrade and identity guides carry migrations; feature docs change inline.
Keep generic proofs in this repository; deployments and their operational
acceptance belong to the downstream operator.

The 2026-10-13 clustering interview brings P2P back to m1 with a required
consumer: application-selected peer paths alongside persistent CDN sessions.
Routing proofs precede the shared wire; federation and P2P ship independently,
while epoch-only active/passive route health follows in m2.

Keep one owner per shared code area. Order each prerequisite before its
consumer; targeted benchmarks and controlled-time regressions remain part of
the feature that needs them.

Added 2026-10-13: moq-transport passthrough, ranked beside interop, keeps
the fields a relay must forward crate-private, an explicit exception to the
public model plan.

Added 2026-10-13 by the redundant TS alignment plan: epoch refusals, the
epoch anchor, and raw TS cuts on the epoch timeline, ranked just above
same-epoch importers, which requires them. Catalog clock adoption ranks
above them.

## Required

- [Epoch correctness](/quest/m0/broadcast-epoch/README.md) - publisher identity, duplicate suppression, and catalog pinning gate the breaking cut
- [Release builds](/quest/m0/release-link.md) - package artifacts can be built
- [qmux credit](/quest/m0/qmux-credit.md) - dropped/stopped streams return credit and close is delivered

- [Restart followers](/quest/m1/restart-followers.md) - first-party consumers follow an epochless `Restart`, from a GOAWAY drain or a publisher replacement, by resolving the path again
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [P2P](/quest/m1/p2p/README.md) - application-selected peer sessions use shared priorities, routing, and same-epoch handover
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation/P2P setup
- [Controlled time](/quest/m1/time/README.md) - every crate and package reads time through moq-clock or @moq/clock, with regression tests for the changed contracts; ranks above the fork, whose switch adopts its instant
- [Hard fork](/quest/m1/quic/fork/README.md) - quinn hard-forked in-tree as `moq-quic`, ranked ahead of perf; the rest of the QUIC line follows it
- [Own the QUIC stack](/quest/m1/quic/README.md) - quinn hard-forked in-tree as `moq-quic`, carrying BBR, reliable reset, hierarchical scheduling, peer limits, and endpoint sharding
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - subgroups, object IDs, properties, end status, datagram objects, and unknown track properties survive moq-transport -> moq-relay (and a relay cluster) -> moq-transport; moxygen's moq-test passes nightly
- [FETCH max-delay](/quest/m1/fetch-max-delay.md) - historical readers obey publisher max-age and an optional tighter reader budget, including while holding a group
- [Draft-20 FETCH edge-case backport](/quest/m1/fetch-edge-cases.md) - backport the final-object and response-group fixes to a later 0.17 patch after main lands
- [Subscribe-close backport](/quest/m1/subscribe-close-backport.md) - the same fix and the `race` loser fix on `release`, so closing a subscription during setup leaves no unhandled rejection in a later 0.17 patch
- [Claim answer epochs](/quest/m1/claim-answer-epochs.md) - a claim answers each output by publishing it at its exact path under its own epoch, so a restarted output never reaches a returning reader as the old one's cache
- [Malformed group FETCH](/quest/m1/group-fetch-malformed.md) - an object missing its Timestamp in a relay's group FETCH ends the whole TIMESCALE track, as subscribe and joining FETCH already do
- [FFI shape](/quest/m1/ffi-shape/README.md) - the bindings mirror Rust's layers: net at the root, then media, json, flate, audio, and video namespaces built from the handle below
- [Bump web-transport-iroh for the capsule close](/quest/m1/iroh-capsule-bump.md) - condition: moq-dev/web-transport#419 ships in a release, then moq's iroh HTTP/3 client reports a peer's close capsule
- [Early drops](/quest/m1/subscribe-drop-early.md) - Rust and JS publishers drop a sequence the moment they give it up, and forward an upstream drop
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - every stream group in a lite subscription arrives or is dropped by name, and lite-07 drops its stream count for it
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [Live largest location](/quest/m1/largest.md) - `largest()` replaces `latest()` in Rust and JS as a live stream of the track's largest location, known before the first group from the publisher's report
- [Live TS clock validation](/quest/m1/ts-live-clock.md) - resolve the local PCR-rate failure with reproducible timing evidence and CI controls
- [Binding panic supervision](/quest/m1/ffi-panic-supervision.md) - a panic in any moq-c or moq-ffi background task reaches the host as a panic, never a lost callback or Cancel
- [Kotlin destroyed arguments](/quest/m1/kt-destroyed-arg.md) - a destroyed object passed to a generated Kotlin call throws instead of aborting the process
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [Plan: a public model](/quest/m1/model-public-surface.md) - every crate-private model item is made public or unnecessary, so a custom publisher or subscriber builds from the public model alone
- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - a `consume` session requests announcements only for prefixes the origin is watching, in JS then Rust; `follow`, `broadcasts`, and the `discovery` option go away
- [Announce caught up](/quest/m1/announce-live.md) - Rust and JS announce consumers expose when a session's initial announce snapshot has arrived, from the counts the wire already carries where a version has them
- [A spinning loop fails a sim test](/quest/m1/spinning-loops.md) - a sim test names any loop that holds a task poll too long, and each one yields through a budget
- [Kotlin wrapper POMs](/quest/m1/kt-ffi-pom.md) - Maven builds of `dev.moq:moq` resolve a published moq-ffi instead of the missing `0.0.0-dev`
- [Epoch anchor](/quest/m1/epoch-anchor.md) - same-epoch importers each derive the catalog clock and timestamp era from the epoch and stay live, so they agree across a 33-bit wrap, a forward clock break, and a restart
- [Raw TS cuts on the epoch timeline](/quest/m1/ts-passthrough-epoch-cuts.md) - passthrough fallback cuts fall on the epoch's grid and the M2ts section derives from the epoch and content, so instances started apart publish identical groups and catalogs
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - importers sharing one `--epoch` and fed one stream publish identical groups and timestamps, so failover between a redundant pair survives
- [Importers announce with a track](/quest/m1/import-declared-track.md) - every importer announces once its catalog names a servable track (a rendition or an extension track like `m2ts`), and input that ends with none exits non-zero unannounced
- [Plan: untimed verbatim PES](/quest/m1/plan-ts-pes-untimed.md) - decide how a verbatim TS track carries a PES that has no PTS, then write the implementation quest
- [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) - json and binary consumers return each value's timestamp, in Rust and every binding; snapshots add `latest()` beside an in-order `next()`
- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - Rust and JS window consumers return each event's frame timestamp like the snapshot and stream consumers
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native FILL_TIMEOUT bounds upstream waiting on drafts 18-22, alongside local content-age enforcement
- [Browser follows a renamed track](/quest/m1/browser-rename-check.md) - a real browser keeps playing when `@moq/publish` renames a track after a codec change or ceiling growth
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` refuses a redefined rendition, and every Rust publisher mints a new rendition name for a decoder identity change and updates everything else in place
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; the catalog entry and format may break in place, since no archives exist
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [IETF epochs](/quest/m1/ietf-epochs.md) - negotiate explicit broadcast identity on drafts 17-22, retaining unchanged paths and epochless clients on updated relays
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [IETF takeover](/quest/m1/ietf-takeover.md) - carry the shared takeover contract through the negotiated IETF epoch extension
- [Identity and recovery guide](/quest/m1/identity-guide.md) - explain pinned catalogs, lazy jobs, takeover, restart and version compatibility
- [Dropped sources](/quest/m1/dropped-sources.md) - track consumers see the producer's real error on every end path, never `Dropped`; a dead route ends Unroutable, and the TS follower lingers on announcements instead of matching errors
- [No made-up Dropped](/quest/m1/dropped-internal.md) - internal code names the real cause instead of `Dropped`, which then only means an app dropped a producer unfinished
- [Client settings parity](/quest/m1/obs-client-config.md) - moq-ffi offers moq-c's client knobs, and the OBS Advanced settings get back the ones the C++ migration dropped
- [Session report parity](/quest/m1/obs-session-report.md) - a session reports its negotiated draft and reconnect failures, so the OBS dock shows them again
- [Generated C++ shape](/quest/m1/cpp-generated-shape.md) - `moq::Client` is the generated type itself and `moq::expected` is one type at every C++ standard
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [BoltFFI or UniFFI](/quest/m1/boltffi.md) - BoltFFI vs UniFFI on a moq-ffi slice in Python, Kotlin, and Swift decides which one the moq-dev fork starts from
- [Hard-fork the binding generator](/quest/m1/uniffi-fork.md) - one moq-dev repo holds the chosen generator's core and every language backend, released under one tag and one pin
- [Binding benchmark harness](/quest/m1/ffi-bench.md) - Python, Kotlin, and Go benchmark their wrappers nightly against two Rust baselines, so FFI overhead is tracked
- [Benchmark trend site](/quest/m1/bench-site.md) - a GitHub Pages site charts the binding, Rust, and JS nightly benchmarks over time
- [Link the benchmark trends](/quest/m1/bench-site-doc.md) - the docs point language choosers at the trend site
- [Swift, Dart, and C++ benchmarks](/quest/m1/ffi-bench-wrappers.md) - the other three wrappers join the nightly binding benchmark
- [Payload bytes](/quest/m1/ffi-bytes.md) - payloads cross the FFI in one bulk copy, proven on the binding benchmark in every wrapper
- [Poll-native async spike](/quest/m1/ffi-poll.md) - measure kio poll-native exports and foreign poll against `ffi::Task`, then pick the shape
- [Rendition preference](/quest/m1/rendition-preference.md) - automatic selection by `<moq-watch>`, `Video::ranked`, and WHEP keeps the highest `preference` that decodes, so a compatibility transcode is only picked when nothing preferred decodes
- [Rust PROBE on demand](/quest/m1/probe-on-demand-rs.md) - a Rust client opens PROBE only while a `recv_bandwidth()` consumer is held, and `Stats` drops `estimated_recv_rate`
- [QoS](/quest/m1/qos/README.md) - broadcast health: relay starvation and timeliness histograms
- [Session outcomes](/quest/m1/session-outcomes.md) - the relay's session stats count refusals by reason and ends by kind, per root and tier, including refusals an embedder records on its stats handle
- [Typed refusal reason](/quest/m1/refusal-reason.md) - an auth server's 403 names the reason (including `expired`) and the root and tier, so the relay's session outcomes count and attribute refusals
- [JS track handover](/quest/m1/js-group-handover.md) - a JS track subscription resumes across a route swap from the first frame it lacks, so `test/drain` passes at zero latency budget
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Plan: watch worker](/quest/m1/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m1/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence, and the media fixture's cold-start audio-tone flake is fixed at its cause
- [JS request deadline](/quest/m1/js-request-deadline.md) - each JS request, PUBLISH_NAMESPACE included, has one fatal 10 s timer from create to answer and fails fast without stream credit, so nothing queues or retries
- [Handshake deadline covers admission](/quest/m1/handshake-admission-deadline.md) - `listen.timeout` bounds a connection through auth admission on both listeners, replacing the hardcoded admission timeout
- [Drain cleanup](/quest/m1/drain-cleanup.md) - sessions force-closed at the drain deadline record their lease end and bytes before the relay or CLI exits, while unfinished handshakes stay bounded
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Rust request credit](/quest/m1/rs-request-credit.md) - a moq-net request fails at once without stream credit instead of waiting
- [qmux no-wait opens](/quest/m1/qmux-no-wait.md) - @moq/qmux rejects an over-limit create when waitUntilAvailable is false, like Chrome
- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - the catalog rotation follows the live camera's orientation
- [Encoder colour](/quest/m1/color-model.md) - every moq-video encode path signals the colour its output actually has, or refuses instead of mislabelling
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [T-STD TS export](/quest/m1/tstd/README.md) - `moq export ts` is a proper remux that passes the supported T-STD buffer models, starting with a fixed `--delay`
- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - a viewer joining at a recovery point drops the leading pictures it cannot decode; continuous viewers keep them
- [CLI stdin shutdown on release](/quest/m1/cli-stdin-release.md) - backport only the runtime-teardown fix so a stopped CLI exits while its stdin writer stays open
- [Draft 14-16 updates](/quest/m1/ietf-legacy-updates.md) - Rust and JS apply and answer moq-transport 14-16 request updates without ending or leaking the request
- [Tail arrivals](/quest/m1/tail-arrivals.md) - a track judges its pending tail from recorded arrivals, not a cache scan, and errors a reader whose tail ends short
- [Resume reorder](/quest/m1/resume-reorder.md) - a group whose stream header arrives after its successor's still resumes on failover; only a dropped or long-gone group fails
- [Refuse rewinds](/quest/m1/refuse-rewinds.md) - a timed track refuses a group whose first frame is earlier than the previous group's, in Rust and js/net, on every hop
- [Feed fetch index](/quest/m1/feed-fetch-index.md) - held feed fetches wake per group and are capped per peer session, on main then release
- [JS pending tail](/quest/m1/js-pending-tail.md) - a JS reader holds for a track's pending tail like Rust
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [A/V sync across a break](/quest/m1/watch-break-av-sync.md) - `@moq/watch` never plays pre-break audio out of sync with pre-break video
- [moqsrc reconnect](/quest/m1/moqsrc-reconnect.md) - `moqsrc` redials after losing its relay and resumes on the same pads
- [Plan: HLS export failure signal](/quest/m1/hls-failure-signal.md) - decide what players see when the exporter's timeline fails, live or history, so a truncated recording never looks complete
- [Close tail drain](/quest/m1/close-tail-drain.md) - after a peer close, subscribers read the group streams their transport already delivered instead of aborting them, so a finished tail arrives on every version
- [Publisher linger](/quest/m1/publisher-linger.md) - every finishing publisher keeps its broadcast up for the media max-age so subscribers can drain it
- [Shared timebase in the bindings](/quest/m1/ffi-timebase.md) - binding apps publish several containers from one source on one shared offset
- [Vulkan Video encode on AMD](/quest/m1/vulkan-encode.md) - H.264 and H.265 from an external Vulkan image on RADV, hand-rolled on ash, proven on an RX 9070
- [VA-API encodes an external Vulkan image](/quest/m1/vaapi-vulkan-import.md) - an explicit-sync DMA-BUF reaches Intel's encoder, proven on Arrow Lake
- [Current LOC Timestamp only](/quest/m1/ietf-loc-timestamp.md) - moq-transport Timestamps are read only from LOC property 0x10, and 0x06 is opaque on every draft
- [Refuse draft-14 END_OF_TRACK properties](/quest/m1/ietf-eot-properties.md) - a draft-14 END_OF_TRACK carrying properties is refused instead of losing them
- [Malformed moq-transport input](/quest/m1/ietf-malformed-close.md) - malformed draft-18 and draft-21 control input closes the session with the draft's code, or PROTOCOL_VIOLATION where a code is a real burden and the fallback is recorded in `doc/concept/standard.md`, in moq-net and js/net
- [Binding track resolver](/quest/m1/binding-track-helper.md) - binding apps subscribe to a rendition without inlining `track` or the key
- [Exporters follow a rendition to a new track](/quest/m1/export-rendition-rebind.md) - the fMP4, TS, MKV and FLV exporters rebuild a rendition whose resolved `track` or `broadcast` changes under the same ID
- [Opaque catalog consumer](/quest/m1/catalog-consumer-opaque.md) - `moq_mux::catalog::Consumer` hides its encoding variants and closed state

- [Stats prefix tracks](/quest/m1/stats-prefix-tracks.md) - additive tracks required by the breaking lag API; existing maps remain supported

- [C++ and OBS package proof](/quest/m1/cpp-package-check.md) - the installed artifacts pass before their first release
- [C++ FFI reuse](/quest/m1/cpp-ffi-reuse.md) - C++ packages link the shared default-feature FFI build once it is proven compatible

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
