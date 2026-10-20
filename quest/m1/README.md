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
  Intel VA-API/Vulkan import, and QoS lag and prefix stats to m2 with their
  prerequisites. Their possible API breaks do not make them prerequisites of
  this cut. Existing playback and A/V clock validation remain here.
  Same-epoch importers were dropped (maintainer, 2026-10-19): a redundant
  ingest pair runs on distinct epochs.
- Finalized lite-07 is preferred by first-party Rust and JS clients, with
  older-version negotiation fallback. Servers accept it by default. Never
  reuse a published identifier for an incompatible wire change; deferred
  features use the appropriate subsequent wip/version if they need one.
- The next cut makes no new AMD/Intel GPU-input support promise. The later
  multi-vendor release needs both hardware proofs before publication.
- Package publication, downstream adoption, and production rollout follow
  the cut under [release execution](/quest/m2/release/README.md).

Existing upgrade and identity guides carry migrations; feature docs change
inline. Keep generic proofs here and deployments and their operational
acceptance with the downstream operator.

Keep one owner per shared code area and order prerequisites before consumers.
The moq-transport passthrough fields remain crate-private, the existing
explicit exception to the public model audit.

Added 2026-10-16: refresh, ranked after moq-transport passthrough, whose
dynamic-groups path it promotes. It is a breaking `Demand` change, so it
lands before the cut.

## Required

- [Epoch correctness](/quest/m0/broadcast-epoch/README.md) - publisher identity, duplicate suppression, and catalog pinning gate the breaking cut
- [Release builds](/quest/m0/release-link.md) - package artifacts can be built
- [qmux credit](/quest/m0/qmux-credit.md) - dropped/stopped streams return credit and close is delivered

- [moq-net follows announced instances](/quest/m1/net-announced-follow.md) - the Rust equivalent of JS `request(path, { announced: true })`, replacing the follow loops in moq-ffi and moq-c
- [Sink restart follow](/quest/m1/sink-restart-follow.md) - moq-cli's server and push sinks follow an epochless `Restart` where their output can mark it, and fail loud otherwise
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [Controlled time](/quest/m1/time/README.md) - every crate and package reads time through moq-clock or @moq/clock, with regression tests for the changed contracts
- [Own the QUIC stack](/quest/m1/quic/README.md) - quinn hard-forked in-tree as `moq-quic`, carrying BBR, reliable reset, hierarchical scheduling, peer limits, and endpoint sharding
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - object IDs, properties, end status, datagram objects, and unknown track properties survive moq-transport -> moq-relay (and a relay cluster) -> moq-transport; moxygen's moq-test sections 1-7 pass nightly, except cases that use a subgroup other than 0
- [Refresh](/quest/m1/refresh/README.md) - a join asks for a fresh group once the current group's deltas exceed a publisher-advertised byte ratio, at every hop; replaces `Demand::used`/`unused` and moq-json's `delta_ratio`
- [Compressed tracks](/quest/m1/compress/README.md) - a track flagged `compress` is DEFLATEd per group by moq-net, forwarded compressed, and inflated once on the relay for peers that cannot signal it; `catalog.json.z` and moq-flate go away
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - waiting on Moxygen to send a registered filter type, then the smoke lane stops hiding its failure
- [Claim answer epochs](/quest/m1/claim-answer-epochs.md) - a claim answers each output by publishing it at its exact path under its own epoch, so a restarted output never reaches a returning reader as the old one's cache
- [Readers see SUBSCRIBE_DROP](/quest/m1/subscribe-drop-errors.md) - a dropped or cut group ends its reader with an error, never a clean end, across cache reclaim, cap changes, and recovery
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [A public model](/quest/m1/public-model/README.md) - every model item a built-in session reaches is public, unnecessary, or marked deliberately private, so a custom publisher, subscriber, or relay builds from the public model alone
- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - a `consume` session requests announcements only for prefixes the origin is watching, in JS then Rust; `follow`, `broadcasts`, and the `discovery` option go away
- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - bindings carry `{value, scale}` timestamps instead of lossy microseconds
- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - Rust and JS window consumers return each event's frame timestamp like the snapshot and stream consumers
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [Report relays that refuse TRACK_STATUS](/quest/m1/track-status-race.md) - moqx and any other relay refusing TRACK_STATUS with an undefined code get an upstream report
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native FILL_TIMEOUT bounds upstream waiting on drafts 18-22, alongside local content-age enforcement
- [Browser follows a renamed track](/quest/m1/browser-rename-check.md) - a real browser keeps playing when `@moq/publish` renames a track after a codec change, while size-only changes keep their identity
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` refuses a redefined rendition, and every Rust publisher mints a new rendition name for a decoder identity change and updates everything else in place
- [One hang catalog consumer](/quest/m1/catalog-consumer-fold.md) - `moq-mux` reads hang catalogs through `hang`'s consumer
- [Broadcast archives](/quest/m1/archive/README.md) - ANNOUNCE, TRACKS, and a SUBSCRIBE per track record every track under a prefix to any object_store, a host serves a stored broadcast back, and the catalog drops its archive pointer for an opt-in `hls` field
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [IETF takeover](/quest/m1/ietf-takeover.md) - carry the shared takeover contract through the negotiated IETF epoch extension
- [Identity and recovery guide](/quest/m1/identity-guide.md) - explain pinned catalogs, lazy jobs, takeover, restart and version compatibility
- [Redundant ingest guide](/quest/m1/redundant-ingest-guide.md) - a setup page on redundant ingest over multiple routes, for any publisher
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [moq-ffi reports why a first dial fails](/quest/m1/ffi-first-dial-failure.md) - OBS names the reason while the first dial retries, as moq-c already can
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, and a shared cluster session is fair across tenants
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence, and the media fixture's cold-start audio-tone flake is fixed at its cause
- [Handshake deadline covers admission](/quest/m1/handshake-admission-deadline.md) - `listen.timeout` bounds a connection through auth admission on both listeners, replacing the hardcoded admission timeout
- [Restart reset](/quest/m1/stateless-reset-key.md) - a relay restarted with the same `listen.reset_key` stateless-resets its predecessor's connections, so clients reconnect within an RTT instead of the idle timeout
- [Drain cleanup](/quest/m1/drain-cleanup.md) - sessions force-closed at the drain deadline record their lease end and bytes before the relay or CLI exits, while unfinished handshakes stay bounded
- [GOAWAY coverage](/quest/m1/goaway-coverage.md) - every IETF request stream handles GOAWAY, a closed control stream is a violation, and the draft-18 cutoff is honored
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Rust request credit](/quest/m1/rs-request-credit.md) - a moq-net request fails at once without stream credit instead of waiting
- [A stalled subscriber holds bounded state](/quest/m1/egress-held-state.md) - group machines and bytes a credit-withholding downstream makes a publisher hold plateau at what its credit and the latency budget allow, on lite and moq-transport
- [qmux no-wait opens](/quest/m1/qmux-no-wait.md) - @moq/qmux rejects an over-limit create when waitUntilAvailable is false, like Chrome
- [Timed-out requests clean up](/quest/m1/legacy-request-timeout-cleanup.md) - on drafts 14-16, Rust and JS send PUBLISH_NAMESPACE_DONE or UNSUBSCRIBE_NAMESPACE for a timed-out request before its local reset
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [Resume reorder](/quest/m1/resume-reorder.md) - a group whose stream header arrives after its successor's still resumes on failover; only a dropped or long-gone group fails
- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - a warm relay serves a reader with a wider max age the cached groups a cold relay would, with no wire change
- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Resumed close](/quest/m1/resume-closed.md) - a resumed JS track keeps its declared end across copies, and its `closed` and `largest()` settle on a clean end
- [Resumed in-flight groups](/quest/m1/resume-in-flight.md) - a replaced route copy stays while a group below its bound is in flight or it backs a handed-out group late, and no longer than the reader's budget
- [Resume through an outage](/quest/m1/resume-outage.md) - a same-epoch resume after an unclosed route dies delivers the backlog its max age still holds, pinned by a regression test
- [Resumed datagrams](/quest/m1/resume-datagrams.md) - a resumed track skips only datagrams it already delivered, not every lower sequence
- [Received timestamps are media time](/quest/m1/lite-time-provenance.md) - a received timed track's timestamps are media time, so the rewind check can run across hops
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [HLS export drains after End](/quest/m1/hls-export-end-drain.md) - a cleanly ended broadcast still exports its retained segments
- [A failed HLS export answers with an error](/quest/m1/hls-failure-signal.md) - playlists answer a no-store 5xx once their timeline fails, and so does the DASH manifest for a shared reference failure, so a truncated recording never looks complete
- [Close tail drain](/quest/m1/close-tail-drain.md) - after a peer close, subscribers read the group streams their transport already delivered instead of aborting them, so a finished tail arrives on every version
- [Binding linger](/quest/m1/linger-ffi.md) - `moq-ffi` and every binding finish a broadcast cleanly, so subscribers drain it until its media is stale
- [JS linger](/quest/m1/linger-js.md) - `js/net` finishes a broadcast cleanly like Rust, and `@moq/publish` uses it when a source stops
- [GStreamer linger](/quest/m1/linger-gst.md) - `moq-gst` sinks finish their broadcast at end of stream instead of closing it
- [Gateway linger](/quest/m1/linger-gateways.md) - HLS, RTMP, SRT and RTC imports linger at a clean end, cut short by a newer epoch at the same path
- [Exporters follow a rendition to a new track](/quest/m1/export-rendition-rebind.md) - the fMP4, TS, MKV and FLV exporters rebuild a rendition whose resolved `track` or `broadcast` changes under the same ID
- [lite-06 max sequence](/quest/m1/lite06-max-sequence.md) - group sequence 2^62-1 reaches a lite-06 subscriber or is refused loudly, instead of vanishing
- [Track live signal](/quest/m1/track-live.md) - a reader can wait for its track's feed to be live, so a player skips old cached groups while a fresh upstream subscription starts
- [moq-mux docs](/quest/m1/docs-mux.md) - every public moq-mux item has rustdoc and the crate warns on missing docs
- [Binding docs](/quest/m1/docs-bindings.md) - every public moq-ffi and moq-c item has rustdoc, carried into the generated bindings and `moq.h`
- [JS export docs](/quest/m1/docs-js.md) - every public JS export and class member has TSDoc, and `just check` enforces it

- [C++ and OBS package proof](/quest/m1/cpp-package-check.md) - the installed artifacts pass before their first release

- [Nightly media players](/quest/m1/media-engines-nightly.md) - the Firefox and WebKit player rows pass, or an engine that cannot play leaves the nightly matrix

- [Dead code and API surface audit](/quest/m1/audit-dead-code.md) - repeat the removal audit before the next major release
- [Dependency and alternatives audit](/quest/m1/audit-deps.md) - dependencies are current, deduplicated, needed, and the best maintained option
- [Test coverage audit](/quest/m1/audit-test-coverage.md) - repeat the coverage audit against the m0 baselines before the next major release
- [Test and CI cost audit](/quest/m1/audit-test-cost.md) - the slowest, flakiest, and least valuable tests and CI jobs are fixed, deleted, or planned
- [Documentation audit](/quest/m1/audit-docs.md) - stale pages, examples, links, and undocumented exports are fixed or planned

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
