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

Added 2026-10-22 (maintainer, security audit of the relay, auth, stats,
ACME, and WebRTC): a clean close misreported as a revocation, injectable
metric tiers, an unbudgeted ACME renewal, unbounded mDNS work per WebRTC
offer, and an undocumented stats linger cut. The audit's `server_name`
finding was already fixed by moq-dev/moq#6509.

Added 2026-10-23 (maintainer, from a linger audit): broadcasts lose their
finished state and retention moves to moq-mux, a break in every binding, so
it lands before the cut.

Added 2026-10-23 (maintainer, from a moq-quic audit): moq-quic and its drivers
are unreleased, so their API is reshaped before the cut publishes it, and the
audit's receive-memory, priority, and lease bugs rank ahead of that reshape.

Added 2026-10-23 (maintainer, pre-cut triage): breaks that would otherwise
force another major (`announce="never"`, the unused OBS outputs, binding
durations, and the remaining web-transport-trait adapters) land before the
cut, as do the small hostile-input and container fixes from the 2026-10-22
security audit, and the stuck-window scan.
Binding durations wait on Duration support in the generated C.

Added 2026-10-24 (maintainer, from a moq-hls audit): import and export
hardening (URL redaction, ranged responses, redirect bases, sequence
arithmetic, DASH init readiness, dot-only names) and ffprobe segment
conformance; the audit's segment-finalization redesign is folded into
Catalog HLS field.

Added 2026-10-25 (maintainer): the studio's
[GPU encode](/quest/m1/studio-gpu/encode.md) and
[source import](/quest/m1/studio-gpu/import.md) quests are in m1 so
moq-video's GPU encode and import API changes, mainly encoders taking a GPU
surface and zero-copy texture import, land before the cut.

## Required

- [moq-congestion](/quest/m1/congestion/README.md) - one QUIC congestion-control crate with a transport-independent trait, run by `moq-quic` and published for noq
- [Release audit findings](/quest/m1/release-audit/README.md) - resolve newly identified resource, parser, playback, and interoperability risks before the cut
- [Cancelled moq-transport requests stop](/quest/m1/ietf-request-cancel.md) - a cancelled request releases its state even when the peer withholds credit
- [Empty segments refused](/quest/m1/empty-segments.md) - `room//cam` fails loud instead of naming `room/cam`
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [No reused track names](/quest/m1/name-reuse.md) - no first-party publisher re-creates a finished track under its old name
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - bindings carry `{value, scale}` timestamps instead of lossy microseconds
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - object IDs, properties, end status, datagram objects, and unknown track properties survive moq-transport -> moq-relay (and a relay cluster) -> moq-transport; moxygen's moq-test sections 1-7 pass nightly under the relay conformance profile, except cases that use a subgroup other than 0
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - a warm relay serves a reader with a wider max age the cached groups a cold relay would, with no wire change
- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Broadcast archives](/quest/m1/archive/README.md) - ANNOUNCE, TRACKS, and a SUBSCRIBE per track record every track under a prefix to any object_store, a host serves a stored broadcast back, and the catalog drops its archive pointer for an opt-in `hls` field
- [Compressed tracks](/quest/m1/compress/README.md) - a track flagged `compress` is DEFLATEd per group by moq-net, forwarded compressed, and inflated once on the relay for peers that cannot signal it; `catalog.json.z` and moq-flate go away
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - waiting on Moxygen to send a registered filter type, then the smoke lane stops hiding its failure
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [Broadcast lifecycle](/quest/m1/broadcast-lifecycle/README.md) - broadcasts are online or offline with no finished state in moq-net, finished-broadcast retention is a fixed moq-mux policy, and each linger timer has one meaning
- [fMP4 passthrough sync start](/quest/m1/fmp4-passthrough-sync.md) - a passthrough video import that starts mid-GOP publishes nothing before its first sync sample
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - every surface added since release has a consumer or is gone, overlapping operations are collapsed, names mirror across Rust, JS, and the bindings, and every break says how to migrate
- [Studio on the GPU](/quest/m1/studio-gpu/README.md) - moq-video encoders take a GPU surface and its renderer reports zero-copy import, proven through moq-studio on NVENC, VideoToolbox, Media Foundation, DMA-BUF, and Metal
- [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md) - drafts 14-17 and 19-21 are deleted from Rust, JS, tests, interop lanes, and docs, removing the legacy control-stream adapter and its request limit
- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - moq-quic and both QUIC drivers ship one mirrored API: `send::`/`recv::` namespaces, one error per direction, consuming terminal operations, plain configs, no quinn baggage
- [Drop web-transport-trait](/quest/m1/drop-web-transport-trait.md) - iroh, wasm, and ffi implement moq-net's `transport::poll` traits, and the remaining `web_transport_trait` adapters are deleted
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native FILL_TIMEOUT bounds upstream waiting on drafts 18 and 22, alongside local content-age enforcement
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` refuses a redefined rendition, and every Rust publisher mints a new rendition name for a decoder identity change and updates everything else in place
- [IETF takeover](/quest/m1/ietf-takeover.md) - carry the shared takeover contract through the negotiated IETF epoch extension
- [Identity and recovery guide](/quest/m1/identity-guide.md) - explain pinned catalogs, lazy jobs, takeover, restart and version compatibility
- [Skip ProbeRTT after a near-floor RTT](/quest/m1/bbr-probe-rtt-skip.md) - BBR3 skips ProbeRTT when an app-limited flow already saw a near-floor RTT, so media stops stalling every 5 s
- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - Rust/FFI and OBS publishers stop encoding the top renditions their grant cannot fund and resume them once probing raises the estimate
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [Lowered resume floor](/quest/m1/resume-floor.md) - lowering a resumed reader's floor re-offers groups only a released copy cached
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [CI housekeeping](/quest/m1/ci-housekeeping.md) - validate queue nextest at 24 threads and remove the actionlint queue ignore after its release
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason, checked by a final paired run before the cut
- [Drop announce="never"](/quest/m1/publish-announce-never.md) - `@moq/publish` offers only announce modes a watcher can reach
- [Drop unused OBS outputs](/quest/m1/obs-output-variants.md) - the plugin registers only `moq_output`, so the broken audio-only variant is gone
- [API Extractor checks](/quest/m1/api-extractor-checks.md) - API Extractor fails a PR on a forgotten export, an unresolved declaration, or an undocumented export, replacing the hand-rolled `.d.ts` checkers
- [Generated JS API reference](/quest/m1/js-api-reference.md) - the doc site renders an API reference for every `@moq/*` package from API Extractor's doc model
- [Interop coverage](/quest/m1/interop-coverage/README.md) - the riskiest moq-transport features moq-net exposes get cross-implementation tests over drafts 18 and 22
- [AMF0 exact reads](/quest/m1/rtmp-amf0-exact-reads.md) - a truncated RTMP string fails with `UnexpectedEof` instead of parsing NUL padding
- [HLS URL redaction](/quest/m1/hls-url-redaction.md) - no moq-hls error prints a signed URL's query or userinfo
- [DASH init readiness](/quest/m1/dash-init-ready.md) - a DASH manifest lists the renditions whose init exists instead of waiting on one that is stuck
- [Binding peer version](/quest/m1/binding-peer-version.md) - every binding names itself ahead of the Rust core in the SETUP Implementation parameter
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - A/A and deliberately slowed Nightly runs set the paired comparison's threshold, so its regression alerts can be trusted
- [HLS export tests over a session](/quest/m1/hls-session-tests.md) - an integration suite records and serves through a real moq-net session, so a publisher disconnect, finish, or takeover ends the cursor and playlists as specified
- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - nightly ffprobe checks that advertised timing, sync flags, and bytes match the media served
- [Unbounded groups](/quest/m1/unbounded-groups/README.md) - groups and frames lose their size caps; the cache sheds parts of a group under pressure and refills the holes in place
- [Metric-safe tiers](/quest/m1/metric-tier.md) - tiers are `[A-Za-z0-9._-]` paths refused where they are set, and every metric label value is escaped

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
