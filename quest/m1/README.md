# m1: clustering stability and the breaking release

## Goal

The next breaking release delivers stable clustered routing and recovery,
switches to moq-quic, and consolidates the planned public API and format
changes. Complete this milestone before cutting main into release.

## Plan

Decided with the maintainer on 2026-10-15: keep a finite next breaking
release, with clustering stability and the in-tree QUIC stack first.

- Preserve epoch correctness, routing recovery, outbound federation, and
  announcement takeover on both lite and IETF. Catalogs, long-lived readers,
  and late joiners obey the same identity contract.
- Keep IETF passthrough, the public model audit, the broad clock conversion,
  QUIC endpoint sharding, archive replay/DVR correctness, and the TS fidelity
  expansion in m1. Keep their correctness, interoperability, memory, and
  performance evidence with them.
- Defer P2P, watch-worker planning and implementation, AMD Vulkan encode and
  Intel VA-API/Vulkan import, QoS lag and prefix stats, and active-active
  same-epoch importers to m2 with their prerequisites. Their possible API
  breaks do not make them prerequisites of this cut. Existing playback and
  A/V clock validation remain here. Same-epoch importers require the epoch
  anchor and raw TS epoch cuts before claiming redundant-import support.
- Finalized lite-07 is preferred by first-party Rust and JS clients, with
  older-version negotiation fallback. Servers accept it by default. Never
  reuse a published identifier for an incompatible wire change; deferred
  features use the appropriate subsequent wip/version if they need one.
- The next cut makes no new AMD/Intel GPU-input support promise. The later
  multi-vendor release needs both hardware proofs before publication.
- Package publication, downstream adoption, and production rollout follow
  the cut under [release execution](/quest/m2/release/README.md).

The route handoff planning quests settle their implementation choices before
code is written. Existing upgrade and identity guides carry migrations;
feature docs change inline. Keep generic proofs here and deployments and
their operational acceptance with the downstream operator.

Keep one owner per shared code area and order prerequisites before consumers.
The moq-transport passthrough fields remain crate-private, the existing
explicit exception to the public model audit.

## Required

- [Epoch correctness](/quest/m0/broadcast-epoch/README.md) - publisher identity, duplicate suppression, and catalog pinning gate the breaking cut
- [Release builds](/quest/m0/release-link.md) - package artifacts can be built
- [qmux credit](/quest/m0/qmux-credit.md) - dropped/stopped streams return credit and close is delivered

- [Per-package MIGRATE.md](/quest/m1/migrate.md) - every published package documents how to migrate each breaking change, and breaking PRs add their entry
- [Restart followers](/quest/m1/restart-followers.md) - first-party consumers follow an epochless `Restart`, from a GOAWAY drain or a publisher replacement, by resolving the path again
- [Catalog version](/quest/m1/catalog-version.md) - the hang catalog names the minimum moq-hang draft a reader needs, and readers refuse a newer one with a typed error
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [Controlled time](/quest/m1/time/README.md) - every crate and package reads time through moq-clock or @moq/clock, with regression tests for the changed contracts
- [Own the QUIC stack](/quest/m1/quic/README.md) - quinn hard-forked in-tree as `moq-quic`, carrying BBR, reliable reset, hierarchical scheduling, peer limits, and endpoint sharding
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - object IDs, properties, end status, datagram objects, and unknown track properties survive moq-transport -> moq-relay (and a relay cluster) -> moq-transport; moxygen's moq-test sections 1-7 pass nightly, except cases that use a subgroup other than 0
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - waiting on Moxygen to send a registered filter type, then the smoke lane stops hiding its failure
- [FETCH max-delay](/quest/m1/fetch-max-delay.md) - historical readers obey publisher max-age and an optional tighter reader budget, including while holding a group
- [Claim answer epochs](/quest/m1/claim-answer-epochs.md) - a claim answers each output by publishing it at its exact path under its own epoch, so a restarted output never reaches a returning reader as the old one's cache
- [FFI shape](/quest/m1/ffi-shape/README.md) - the bindings mirror Rust's layers: net at the root, then media, json, flate, audio, and video namespaces built from the handle below
- [Early drops](/quest/m1/subscribe-drop-early.md) - Rust and JS publishers drop a sequence the moment they give it up, and forward an upstream drop
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - every stream group in a lite subscription arrives or is dropped by name, and lite-07 drops its stream count for it
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [Live largest location](/quest/m1/largest.md) - `largest()` replaces `latest()` in Rust and JS as a live stream of the track's largest location, known before the first group from the publisher's report
- [Live TS clock validation](/quest/m1/ts-live-clock.md) - resolve the local PCR-rate failure with reproducible timing evidence and CI controls
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [A public model](/quest/m1/public-model/README.md) - every model item a built-in session reaches is public, unnecessary, or marked deliberately private, so a custom publisher, subscriber, or relay builds from the public model alone
- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - a `consume` session requests announcements only for prefixes the origin is watching, in JS then Rust; `follow`, `broadcasts`, and the `discovery` option go away
- [Announce caught up](/quest/m1/announce-live.md) - Rust and JS announce consumers expose when a session's initial announce snapshot has arrived, from the counts the wire already carries where a version has them
- [A spinning loop fails a sim test](/quest/m1/spinning-loops.md) - a sim test names any loop that holds a task poll too long, and each one yields through a budget
- [Importers announce with a track](/quest/m1/import-declared-track.md) - every importer announces once its catalog names a servable track (a rendition or an extension track like `m2ts`), and input that ends with none exits non-zero unannounced
- [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) - json and binary consumers return each value's timestamp, in Rust and every binding; snapshots add `latest()` beside an in-order `next()`
- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - Rust and JS window consumers return each event's frame timestamp like the snapshot and stream consumers
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native FILL_TIMEOUT bounds upstream waiting on drafts 18-22, alongside local content-age enforcement
- [Browser follows a renamed track](/quest/m1/browser-rename-check.md) - a real browser keeps playing when `@moq/publish` renames a track after a codec change, while size-only changes keep their identity
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` refuses a redefined rendition, and every Rust publisher mints a new rendition name for a decoder identity change and updates everything else in place
- [Archive](/quest/m1/archive/README.md) - record selected tracks to any object_store and replay them over FETCH or derived HLS; preserve published format boundaries while completing replay and DVR correctness
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [IETF epochs](/quest/m1/ietf-epochs.md) - negotiate explicit broadcast identity on drafts 17-22, retaining unchanged paths and epochless clients on updated relays
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [IETF takeover](/quest/m1/ietf-takeover.md) - carry the shared takeover contract through the negotiated IETF epoch extension
- [Identity and recovery guide](/quest/m1/identity-guide.md) - explain pinned catalogs, lazy jobs, takeover, restart and version compatibility
- [Dropped sources](/quest/m1/dropped-sources.md) - track consumers see the producer's real error on every end path, never `Dropped`; a dead route ends Unroutable, and the TS follower lingers on announcements instead of matching errors
- [Client settings parity](/quest/m1/obs-client-config.md) - moq-ffi offers moq-c's client knobs, and the OBS Advanced settings get back the ones the C++ migration dropped
- [Session report parity](/quest/m1/obs-session-report.md) - a session reports its negotiated draft and reconnect failures, so the OBS dock shows them again
- [Generated C++ shape](/quest/m1/cpp-generated-shape.md) - `moq::Client` is the generated type itself and `moq::expected` is one type at every C++ standard
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [Rust PROBE on demand](/quest/m1/probe-on-demand-rs.md) - a Rust client opens PROBE only while a `recv_bandwidth()` consumer is held, and `Stats` drops `estimated_recv_rate`
- [Session outcomes](/quest/m1/session-outcomes.md) - the relay's session stats count refusals by reason and ends by kind, per root and tier, including refusals an embedder records on its stats handle
- [Typed refusal reason](/quest/m1/refusal-reason.md) - an auth server's 403 names the reason (including `expired`) and the root and tier, so the relay's session outcomes count and attribute refusals
- [JS track handover](/quest/m1/js-group-handover.md) - a JS track subscription resumes across a route swap from the first frame it lacks, so `test/drain` passes at zero latency budget
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence, and the media fixture's cold-start audio-tone flake is fixed at its cause
- [JS request deadline](/quest/m1/js-request-deadline.md) - each JS request, PUBLISH_NAMESPACE included, has one fatal 10 s timer from create to answer and fails fast without stream credit, so nothing queues or retries
- [Handshake deadline covers admission](/quest/m1/handshake-admission-deadline.md) - `listen.timeout` bounds a connection through auth admission on both listeners, replacing the hardcoded admission timeout
- [Drain cleanup](/quest/m1/drain-cleanup.md) - sessions force-closed at the drain deadline record their lease end and bytes before the relay or CLI exits, while unfinished handshakes stay bounded
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Rust request credit](/quest/m1/rs-request-credit.md) - a moq-net request fails at once without stream credit instead of waiting
- [qmux no-wait opens](/quest/m1/qmux-no-wait.md) - @moq/qmux rejects an over-limit create when waitUntilAvailable is false, like Chrome
- [Encoder colour](/quest/m1/color-model.md) - every moq-video encode path signals the colour its output actually has, or refuses instead of mislabelling
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [T-STD TS export](/quest/m1/tstd/README.md) - `moq export ts` is a proper remux that passes the supported T-STD buffer models, starting with a fixed `--delay`
- [Draft 14-16 updates](/quest/m1/ietf-legacy-updates.md) - Rust and JS apply and answer moq-transport 14-16 request updates without ending or leaking the request
- [Tail arrivals](/quest/m1/tail-arrivals.md) - a track judges its pending tail from recorded arrivals, not a cache scan, and errors a reader whose tail ends short
- [Resume reorder](/quest/m1/resume-reorder.md) - a group whose stream header arrives after its successor's still resumes on failover; only a dropped or long-gone group fails
- [Refuse rewinds](/quest/m1/refuse-rewinds.md) - a timed track refuses a group whose first frame is earlier than the previous group's, in Rust and js/net, on every hop
- [JS pending tail](/quest/m1/js-pending-tail.md) - a JS reader holds for a track's pending tail like Rust
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [Plan: HLS export failure signal](/quest/m1/hls-failure-signal.md) - decide what players see when the exporter's timeline fails, live or history, so a truncated recording never looks complete
- [Close tail drain](/quest/m1/close-tail-drain.md) - after a peer close, subscribers read the group streams their transport already delivered instead of aborting them, so a finished tail arrives on every version
- [Publisher linger](/quest/m1/publisher-linger.md) - every finishing publisher keeps its broadcast up for the media max-age so subscribers can drain it
- [Malformed moq-transport input](/quest/m1/ietf-malformed-close.md) - malformed draft-18 and draft-21 control input closes the session with the draft's code, or PROTOCOL_VIOLATION where a code is a real burden and the fallback is recorded in `doc/concept/standard.md`, in moq-net and js/net
- [Binding track resolver](/quest/m1/binding-track-helper.md) - binding apps subscribe to a rendition without inlining `track` or the key
- [Exporters follow a rendition to a new track](/quest/m1/export-rendition-rebind.md) - the fMP4, TS, MKV and FLV exporters rebuild a rendition whose resolved `track` or `broadcast` changes under the same ID

- [C++ and OBS package proof](/quest/m1/cpp-package-check.md) - the installed artifacts pass before their first release

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
