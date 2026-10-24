# m1: clustering stability and the next release

## Goal

Everything that lands before `main` is next cut into `release`: stable
clustered routing and recovery, the switch to moq-quic, the planned public
API and format changes, and the non-breaking fixes and checks queued ahead of
the cut. Complete this milestone before cutting main into release.

## Plan

Decided with the maintainer on 2026-10-15: clustering stability and the
in-tree QUIC stack first. Decided 2026-10-20 (maintainer): m1 is everything
before the cut, not only breaking work, so ready non-breaking quests stay
here instead of moving out; the cut is what ships them.

- Preserve epoch correctness, routing recovery, outbound federation, and
  announcement takeover on both lite and IETF. Catalogs, long-lived readers,
  and late joiners obey the same identity contract.
- Keep IETF passthrough, the API audit, the broad clock conversion,
  archive replay/DVR correctness, and the TS fidelity
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

Keep one owner per shared code area and order prerequisites before consumers
(re-ranked in the 2026-10-20 audit: auth before cluster routing, the C line
before the binding breaks, and every lite-07 prerequisite before those
quests).
The moq-transport passthrough fields remain crate-private: no public moq-net
item exists only for moq-transport (maintainer, 2026-10-23).

Added 2026-10-16: refresh, a breaking `Demand` change, so it lands before
the cut.

Added 2026-10-22 (maintainer, from an external API audit): small breaks
that would otherwise force another breaking release (the FFI encoder config,
FromStr, dead items, `SessionClosed`, unprefixed C++), the hang draft's
warmup rule for the shipping dual decoder, the binding migration guide fixes,
and the API-diff tooling. Public routes now wait on cluster routing.

Added 2026-10-20 (maintainer): the probe target, native probing, and the
native rung disable moved up from the [m2 probe epic](/quest/m2/probe/README.md),
so Rust, FFI, and OBS publishers recover a stopped rendition in this release;
padding and the browser's rung disable stay in m2, with only padding's
lite-07 stream type here.

Added 2026-10-22 (maintainer, clustering audit): three interim fixes for
announce churn on today's hop-list mesh, shipping with the next cut from
`main` (not backported to `release`), so deployments get relief before the
route layer ships; and stats groups as
tracks, so per-group stats stop costing cluster announces.

Added 2026-10-23 (maintainer, from a linger audit): broadcasts lose their
finished state and retention moves to moq-mux, a break in every binding, so
it lands before the cut.

Added 2026-10-23 (maintainer, from a moq-quic audit): moq-quic and its drivers
are unreleased, so their API is reshaped before the cut publishes it, and the
audit's receive-memory, priority, and lease bugs rank ahead of that reshape.

## Required

- [Epoch correctness](/quest/m0/broadcast-epoch/README.md) - publisher identity, duplicate suppression, and catalog pinning gate the breaking cut
- [moq-congestion](/quest/m1/congestion/README.md) - one QUIC congestion-control crate with a transport-independent trait, run by `moq-quic` and published for noq
- [Release audit findings](/quest/m1/release-audit/README.md) - resolve newly identified resource, parser, playback, and interoperability risks before the cut
- [SUBSCRIBE_TRACKS follows credit](/quest/m1/listing-credit.md) - a listing holds state in proportion to what the peer reads, not to the catalog
- [Cancelled moq-transport requests stop](/quest/m1/ietf-request-cancel.md) - a cancelled request releases its state even when the peer withholds credit
- [SUBSCRIBE setup sees a cancel](/quest/m1/ietf-setup-cancel.md) - a subscriber that cancels mid-setup releases upstream demand and its slot at once
- [JS qmux stream credit](/quest/m1/js-qmux-credit.md) - credit returns when both halves close, so `@moq/net` drops its caps
- [Every close records its cause](/quest/m1/close-cause.md) - protocol-violation closes and abandoned SETUP replies record why
- [Empty segments refused](/quest/m1/empty-segments.md) - `room//cam` fails loud instead of naming `room/cam`
- [Readers follow restarts](/quest/m1/sink-restart-follow.md) - every moq-cli and moq-gst reader, moqsrc and `moq play` included, follows instances through `announced_broadcast` and an epochless `Restart` where its output can mark it; otherwise it stays on the old instance and fails once that route dies
- [Readers follow a moved rendition](/quest/m1/rendition-move-follow.md) - moq-rtc, moq-ffi, moq-gst and `moq play` follow a rendition the catalog moves to a new track
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [Stats groups as tracks](/quest/m1/stats-group-tracks.md) - one stable node broadcast per stats producer, with each group a produced track listed by TRACKS
- [No reused track names](/quest/m1/name-reuse.md) - no first-party publisher re-creates a finished track under its old name
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [Controlled time](/quest/m1/time/README.md) - every crate and package reads time through moq-clock or @moq/clock, with regression tests for the changed contracts
- [Receive pinning](/quest/m1/quic-receive-pinning.md) - tiny DATAGRAM or STREAM frames cannot pin moq-quic receive batches past the receive budgets
- [Priority requeue](/quest/m1/quic-priority-requeue.md) - raising a queued QUIC stream's priority moves it at once instead of after it next sends
- [Lease bookkeeping](/quest/m1/quic-lease-bookkeeping.md) - receive leases and the connection window keep exact books, and eviction stops scanning every stream
- [io_uring listen extensions](/quest/m1/uring-listen-extensions.md) - ring listeners offer the configured `[listen] extensions`
- [Rust request credit](/quest/m1/rs-request-credit.md) - a moq-net request fails at once without stream credit instead of waiting
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - bindings carry `{value, scale}` timestamps instead of lossy microseconds
- [FFI shutdown requests the drain synchronously](/quest/m1/ffi-shutdown-sync.md) - `shutdown()` becomes a sync drain request awaited with `closed()`, so a dropped future can no longer cancel
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - object IDs, properties, end status, datagram objects, and unknown track properties survive moq-transport -> moq-relay (and a relay cluster) -> moq-transport; moxygen's moq-test sections 1-7 pass nightly under the relay conformance profile, except cases that use a subgroup other than 0
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [Updates reach fills](/quest/m1/fill-updates.md) - a reader's new priority, delay, or order reaches fills already queued or taken
- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - a warm relay serves a reader with a wider max age the cached groups a cold relay would, with no wire change
- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Broadcast archives](/quest/m1/archive/README.md) - ANNOUNCE, TRACKS, and a SUBSCRIBE per track record every track under a prefix to any object_store, a host serves a stored broadcast back, and the catalog drops its archive pointer for an opt-in `hls` field
- [A listing's clean end keeps its finishes](/quest/m1/listing-clean-end.md) - a broadcast ending right after its tracks finish ends each finished track's IETF PUBLISH with TRACK_ENDED, in Rust and JS
- [Compressed tracks](/quest/m1/compress/README.md) - a track flagged `compress` is DEFLATEd per group by moq-net, forwarded compressed, and inflated once on the relay for peers that cannot signal it; `catalog.json.z` and moq-flate go away
- [The lite draft agrees that a forbidden value closes the session](/quest/m1/lite-draft-close-rules.md) - the frame-bound and lite-06 Timescale rules say close the session, matching the code and the Message Length rule
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - waiting on Moxygen to send a registered filter type, then the smoke lane stops hiding its failure
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [Broadcast lifecycle](/quest/m1/broadcast-lifecycle/README.md) - broadcasts are online or offline with no finished state in moq-net, finished-broadcast retention is a fixed moq-mux policy, and each linger timer has one meaning
- [JS interop tone](/quest/m1/interop-js-tone.md) - the `js ->` cells pass the audio check on every draft again, after the 2026-10-23 regression
- [fMP4 edit lists](/quest/m1/fmp4-edit-lists.md) - the fMP4 importer applies edit lists to frame timestamps, so the interop AAC tone passes the phase check
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - every surface added since release has a consumer or is gone, overlapping operations are collapsed, names mirror across Rust, JS, and the bindings, and every break says how to migrate
- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - moq-quic and both QUIC drivers ship one mirrored API: `send::`/`recv::` namespaces, one error per direction, consuming terminal operations, plain configs, no quinn baggage
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native FILL_TIMEOUT bounds upstream waiting on drafts 18-22, alongside local content-age enforcement
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` refuses a redefined rendition, and every Rust publisher mints a new rendition name for a decoder identity change and updates everything else in place
- [IETF takeover](/quest/m1/ietf-takeover.md) - carry the shared takeover contract through the negotiated IETF epoch extension
- [Identity and recovery guide](/quest/m1/identity-guide.md) - explain pinned catalogs, lazy jobs, takeover, restart and version compatibility
- [Native probing](/quest/m1/probe-native.md) - moq-quic retransmits in-flight data during BBR3's ProbeBW_UP so an app-limited sender validates headroom up to the target
- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - Rust/FFI and OBS publishers stop encoding the top renditions their grant cannot fund and resume them once probing raises the estimate
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [Play keeps its audio output](/quest/m1/play-audio-handover.md) - `moq play` switches to a replacement run without reopening its audio output, so native-play holds its derived bound
- [Audio playout memory](/quest/m1/audio-playout-memory.md) - a catalog's advertised jitter or delay cannot size decoded PCM; the lookahead stays encoded
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [Buffered video poster](/quest/m1/buffered-video-poster.md) - buffered startup paints the first video frame at once and starts A/V together at a shared floor
- [JS reload config](/quest/m1/js-reload-config.md) - `@moq/net` reload takes Rust's `healthy` backoff and no ignored `extensions`
- [JS release preflight](/quest/m1/js-publish-preflight.md) - `release.ts` refuses a package whose `@moq/*` dependency is not on npm, so no dependent publishes uninstallable
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [Encoders declare no reordering](/quest/m1/encoder-reorder.md) - moq-video encoders publish `reorder: 0`
- [Rust resume checks compression](/quest/m1/resume-compress.md) - a same-epoch route copy with a different `compress` is refused instead of corrupting framing
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [Producers drop the phantom catalog type](/quest/m1/catalog-phantom.md) - moq-video, moq-audio and moq-mux producers lose an `E: CatalogExt` parameter that only `PhantomData` holds
- [JSON encoder options without a downcast](/quest/m1/json-encoder-options.md) - moq-mux's `json_snapshot` takes its `deltas` switch through a typed path any custom catalog entry can use, not an `Any` downcast
- [CI housekeeping](/quest/m1/ci-housekeeping.md) - validate queue nextest at 24 threads and remove the actionlint queue ignore after its release
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason, checked by a final paired run before the cut
- [Subscribe update camera check](/quest/m1/subscribe-update-churn.md) - a person checks a physical-camera publish for SUBSCRIBE_UPDATE chatter; bbb and the fake camera measured quiet
- [Unprefixed C++ free functions](/quest/m1/cpp-free-function-prefix.md) - generated free functions drop `moq_`, and `moq.hpp` deletes its five pass-through wrappers before C++ first publishes
- [Error cleanups](/quest/m1/error-cleanups.md) - delete the unused `Error::SessionClosed`; raw transport failures already map to `Unroutable`
- [API-diff check](/quest/m1/api-diff.md) - a recipe diffs every published package's API against its last release, run on PRs into release and against main before the cut
- [API Extractor checks](/quest/m1/api-extractor-checks.md) - API Extractor fails a PR on a forgotten export, an unresolved declaration, or an undocumented export, replacing the hand-rolled `.d.ts` checkers
- [Generated JS API reference](/quest/m1/js-api-reference.md) - the doc site renders an API reference for every `@moq/*` package from API Extractor's doc model
- [IETF lazy discovery](/quest/m1/ietf-lazy-discovery.md) - moq-transport sessions ask for announcements only under watched prefixes, starting with UNSUBSCRIBE_NAMESPACE on d14/15
- [Interop coverage](/quest/m1/interop-coverage/README.md) - the riskiest moq-transport features moq-net exposes get cross-implementation tests over the even drafts 14 to 22
- [One Annex-B exporter](/quest/m1/annexb-export.md) - H.264 and H.265 Annex-B export share one private lifecycle, which also follows a rebound rendition
- [Caption store bounds](/quest/m1/caption-store-bounds.md) - a hostile caption track cannot grow a viewer's cue and region store or its per-frame work without bound
- [Peers advertise their implementation and version](/quest/m1/peer-version.md) - moq-net and `@moq/net` send a `name/version` SETUP parameter both ways, and the auth request and session span carry the client's
- [Binding peer version](/quest/m1/binding-peer-version.md) - every binding names itself ahead of the Rust core in that parameter
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - A/A and deliberately slowed Nightly runs set the paired comparison's threshold, so its regression alerts can be trusted
- [Interop audit](/quest/m1/audit-interop.md) - our interop runner entries match what we ship, and every red interop cell with a moq.dev side is fixed, reported, or noted as down
- [Lint override and deprecation audit](/quest/m1/audit-lints.md) - repeat the suppression and deprecation audit, starting with the conditional allows and the lanes clippy never runs
- [Refuse unknown MOQ_* environment variables](/quest/m1/env-unknown.md) - every binary exits at startup naming a `MOQ_*` variable it does not read
- [HLS recorder in the CLI](/quest/m1/hls-record-cli.md) - `export hls --output <dir>` records playlists and segments to a directory
- [Group storage benchmark](/quest/m1/group-storage-bench.md) - numbers pick the append-only log, a ring buffer, or sorted runs for `GroupState` before either lands
- [Unbounded groups](/quest/m1/unbounded-groups/README.md) - groups and frames lose their size caps; the cache sheds parts of a group under pressure and refills the holes in place

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
