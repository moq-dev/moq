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
- Keep IETF passthrough, the public model audit, the broad clock conversion,
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
before the binding breaks, and every lite-07 and public model prerequisite
before those quests).
The moq-transport passthrough fields remain crate-private, the existing
explicit exception to the public model audit.

Added 2026-10-16: refresh, ranked after moq-transport passthrough, whose
dynamic-groups path it promotes. It is a breaking `Demand` change, so it
lands before the cut.

Added 2026-10-20 (maintainer): the probe target, native probing, and the
native rung disable moved up from the [m2 probe epic](/quest/m2/probe/README.md),
so Rust, FFI, and OBS publishers recover a stopped rendition in this release;
padding and the browser's rung disable stay in m2, with only padding's
lite-07 stream type here.

## Required

- [Epoch correctness](/quest/m0/broadcast-epoch/README.md) - publisher identity, duplicate suppression, and catalog pinning gate the breaking cut
- [Readers follow restarts](/quest/m1/sink-restart-follow.md) - every moq-cli and moq-gst reader, moqsrc and `moq play` included, follows instances through `announced_broadcast` and an epochless `Restart` where its output can mark it; otherwise it stays on the old instance and fails once that route dies
- [In-band auth](/quest/m1/auth/README.md) - a session tells its peer what it may publish and subscribe to, unions tokens presented in band, and fails loud on an out-of-scope publish
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [Controlled time](/quest/m1/time/README.md) - every crate and package reads time through moq-clock or @moq/clock, with regression tests for the changed contracts
- [Own the QUIC stack](/quest/m1/quic/README.md) - quinn hard-forked in-tree as `moq-quic`, carrying BBR, reliable reset, hierarchical scheduling, peer limits, and endpoint sharding
- [qmux on the QUIC stream state machine](/quest/m1/quic-qmux.md) - qmux runs in-tree over `moq-quic`'s stream state machine, returning credit for every byte, delivering its close frame, and ending an unfinished read after its buffered data once the session closes
- [lite-07 padding](/quest/m1/lite07-padding.md) - lite-07 reserves a padding stream type that receivers read and discard, for the browser's padding probes
- [Rust request credit](/quest/m1/rs-request-credit.md) - a moq-net request fails at once without stream credit instead of waiting
- [Generated C bindings](/quest/m1/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written libmoq
- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - bindings carry `{value, scale}` timestamps instead of lossy microseconds
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - object IDs, properties, end status, datagram objects, and unknown track properties survive moq-transport -> moq-relay (and a relay cluster) -> moq-transport; moxygen's moq-test sections 1-7 pass nightly, except cases that use a subgroup other than 0
- [Refresh](/quest/m1/refresh/README.md) - a join asks for a fresh group once the current group's deltas exceed a publisher-advertised byte ratio, at every hop; replaces `Demand::used`/`unused` and moq-json's `delta_ratio`
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - a warm relay serves a reader with a wider max age the cached groups a cold relay would, with no wire change
- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Broadcast archives](/quest/m1/archive/README.md) - ANNOUNCE, TRACKS, and a SUBSCRIBE per track record every track under a prefix to any object_store, a host serves a stored broadcast back, and the catalog drops its archive pointer for an opt-in `hls` field
- [Compressed tracks](/quest/m1/compress/README.md) - a track flagged `compress` is DEFLATEd per group by moq-net, forwarded compressed, and inflated once on the relay for peers that cannot signal it; `catalog.json.z` and moq-flate go away
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - waiting on Moxygen to send a registered filter type, then the smoke lane stops hiding its failure
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the second round of load-only failures, one quest per flake, fixed at the cause
- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - a `consume` session requests announcements only for prefixes the origin is watching, in JS then Rust; `follow`, `broadcasts`, and the `discovery` option go away
- [A public model](/quest/m1/public-model/README.md) - every model item a built-in session reaches is public, unnecessary, or marked deliberately private, so a custom publisher, subscriber, or relay builds from the public model alone
- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - Rust and JS window consumers return each event's frame timestamp like the snapshot and stream consumers
- [FETCH_OK whole group](/quest/m1/ietf-fetch-end-location.md) - on drafts 14 to 19, FETCH_OK End Object 0 means the whole group in both directions, and a FETCH ending at the maximum group is refused as unsupported
- [Pause an IETF SUBSCRIBE](/quest/m1/ietf-subscribe-pause.md) - Forward=0 pauses a SUBSCRIBE without demand upstream and Forward=1 resumes it, instead of ending it
- [JavaScript FETCH](/quest/m1/js-fetch.md) - browser publishers answer IETF FETCH through the JS ranges request surface
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native FILL_TIMEOUT bounds upstream waiting on drafts 18-22, alongside local content-age enforcement
- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` refuses a redefined rendition, and every Rust publisher mints a new rendition name for a decoder identity change and updates everything else in place
- [IETF takeover](/quest/m1/ietf-takeover.md) - carry the shared takeover contract through the negotiated IETF epoch extension
- [Identity and recovery guide](/quest/m1/identity-guide.md) - explain pinned catalogs, lazy jobs, takeover, restart and version compatibility
- [Scope track priority](/quest/m1/track-priority-scope.md) - priority orders one owner's streams, sent and received, and a shared cluster session is fair across tenants
- [An evicted group is judged stale](/quest/m1/evicted-group-stale.md) - a handed-out group the cache evicted goes stale once its successor's arrival plus the budget passes, so its stalled stream resets
- [Native probing](/quest/m1/probe-native.md) - moq-quic retransmits in-flight data during BBR3's ProbeBW_UP so an app-limited sender validates headroom up to the target
- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - Rust/FFI and OBS publishers stop encoding the top renditions their grant cannot fund and resume them once probing raises the estimate
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - forcing a channel count on an Audio.Capture stops costing the subscriber gaps of silence, and the media fixture's cold-start audio-tone flake is fixed at its cause
- [moq --listen admission test](/quest/m1/listen-admission-test.md) - an in-process test proves `moq --listen` bounds admission by `listen.timeout`
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [qmux no-wait opens](/quest/m1/qmux-no-wait.md) - the in-tree @moq/qmux rejects an over-limit create when waitUntilAvailable is false, like Chrome
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [Resumed close](/quest/m1/resume-closed.md) - a resumed JS track keeps its declared end across copies, and its `closed` and `largest()` settle on a clean end
- [Replica resume](/quest/m1/replica-resume.md) - a lagging same-epoch replica resumes with every frame exactly once and in order, on lite-07 and on moq-transport
- [Rust resume holds a declared end](/quest/m1/resume-rust-end.md) - a resumed Rust track keeps a declared end across copies, dedupes flapping copies, and judges retention inside the reader's window
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [Binding linger](/quest/m1/linger-ffi.md) - `moq-ffi` and every binding finish a broadcast cleanly, so subscribers drain it until its media is stale
- [Interop finish](/quest/m1/interop-finish.md) - the interop matrix proves a clean broadcast finish across implementations
- [Exporters follow a rendition to a new track](/quest/m1/export-rendition-rebind.md) - the fMP4, TS, MKV and FLV exporters rebuild a rendition whose resolved `track` or `broadcast` changes under the same ID
- [fMP4 import unwraps to Legacy](/quest/m1/fmp4-import-unwrap.md) - fMP4 and HLS imports publish per-sample `Legacy` frames unless `--passthrough` asks for CMAF, and no first-party producer defaults to CMAF
- [lite-06 max sequence](/quest/m1/lite06-max-sequence.md) - group sequence 2^62-1 reaches a lite-06 subscriber or is refused loudly, instead of vanishing
- [Track live signal](/quest/m1/track-live.md) - a reader can wait for its track's feed to be live, so a player skips old cached groups while a fresh upstream subscription starts
- [moq-net compiles once](/quest/m1/net-erase-transport.md) - the session drivers stop monomorphizing per transport and per crate, saving several MiB of the relay, FFI library, `libmoq` and CLI
- [Audio devices on ARM](/quest/m1/audio-devices-arm.md) - `just rs audio-devices` passes on the hosted ARM runner
- [CI housekeeping](/quest/m1/ci-housekeeping.md) - validate queue nextest at 24 threads and remove the actionlint queue ignore after its release
- [Interop audio delivery](/quest/m1/interop-audio.md) - every interop subscriber decodes and checks the publishers' 440 Hz tone, and the Rust publisher sends Opus
- [AAC in the native JS fallback](/quest/m1/libav-aac.md) - Node and Bun players decode AAC, and the Rust interop publisher adds an AAC track
- [Native audio decode conformance](/quest/m1/audio-decode-conformance.md) - Opus and AAC decode through every moq-audio backend, moq-c, and moq-ffi is checked against known tones and clips, like moq-video's suite
- [Allocation-free single-range subscriber window](/quest/m1/window-alloc.md) - a one-range subscribe resolves its window without a heap allocation, recovering the join cost per-subscriber ranges added
- [moq-net internals](/quest/m1/net-internals.md) - no per-group session clones, no transport type parameter in lite and IETF internals, and the `Local` check measured
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason, checked by a final paired run before the cut
- [Subscribe update churn](/quest/m1/subscribe-update-churn.md) - a viewer sends SUBSCRIBE_UPDATE only on a meaningful change, measured after the publisher stall fix
- [Interop coverage](/quest/m1/interop-coverage/README.md) - the riskiest moq-transport features moq-net exposes get cross-implementation tests over the even drafts 14 to 22
- [Runner nightly](/quest/m1/interop-runner-nightly.md) - a nightly job runs the upstream interop runner against our relay and clients, so newly registered implementations with a published image are tested without a change here
- [CI workflow audit](/quest/m1/audit-ci.md) - every workflow and job gives a signal someone sees, at a cost that signal justifies, or is fixed, moved, or deleted
- [Artifact size audit](/quest/m1/audit-size.md) - binary, FFI, WASM, and bundle growth since the m0 run is explained, fixed, or planned
- [Compile time audit](/quest/m1/audit-compile-time.md) - build-time hotspots are measured against the m0 baselines and the worst are fixed or planned
- [Performance audit](/quest/m1/audit-perf.md) - benchmarks that regressed since the m0 run are explained, fixed, or planned, and unbenched hot paths get coverage
- [Quest tree audit](/quest/m1/audit-quests.md) - the tree agrees with itself and with main

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
