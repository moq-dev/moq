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
(re-ranked in the 2026-10-20 audit: auth before cluster routing, and every
lite-07 prerequisite before those quests).
Decided 2026-10-25 (maintainer): the hand-written `moq-c` ships at this cut
with parity to moq-ffi for every m1 binding feature, so each binding quest
lands its C half there; the generated C line moves after the cut.
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

Added 2026-10-24 (maintainer, from a moq-hls audit): import and export
hardening (URL redaction, ranged responses, redirect bases, sequence
arithmetic, DASH init readiness, dot-only names) and ffprobe segment
conformance; the audit's segment-finalization redesign is folded into
Catalog HLS field.

Added 2026-10-24 (maintainer-requested pre-cut audit of main): remote
crashes and amplification in media and RTMP parsing, lite announce bounds,
JS track terminal causes, browser element teardown, and smaller Rust/JS
divergences, under one epic; its redesign proposals went to m2.

Changed 2026-10-26 (maintainer): pure-Rust crypto moved to m3 until upstream
graviola and ACME releases make it cheap, so the cut ships on aws-lc-rs.
Dropping the `rsa` crate from moq-auth's production graph stays in m1.

Added 2026-10-25 (maintainer): the studio's
GPU encode and source import quests are in m1 so
moq-video's GPU encode and import API changes, mainly encoders taking a GPU
surface and zero-copy texture import, land before the cut.

Added 2026-10-27 (maintainer, from an external security audit): relay and
wire boundary fixes, importer streaming, moq-c handle wrap, browser input
checks, and CI tool pins, under one epic; its credential leaks went to m0.

Added 2026-10-27 (maintainer, from an external release-to-main audit): the
bindings' silent `max_delay` default change and the upgrade notes that
contradict the code. Its other findings already had owners or contradicted
settled decisions (archives break in place; OBS video-only output stays
removed).

## Required

- [Address groups by timestamp](/quest/m1/time-addressing/README.md) - group start timestamps replace sequence numbers in the moq-net API and lite-07 wire, while old subscribers, old publishers, and published JS clients keep working
- [Release audit findings](/quest/m1/release-audit/README.md) - resolve newly identified resource, parser, playback, and interoperability risks before the cut
- [Pre-cut audit findings](/quest/m1/cut-audit/README.md) - fix the crashes, hostile-input costs, and correctness bugs the 2026-10-24 audit of main found
- [moq-quic has every quinn fix it needs](/quest/m1/quinn-sync.md) - quinn's fixes since the fork point are ported with their tests, once per release cycle
- [Security audit findings](/quest/m1/security-audit/README.md) - fix the verified relay, importer, moq-c, browser, and CI findings of the 2026-10-27 external security audit
- [Withdrawn advertisements stop](/quest/m1/ietf-withdraw-credit.md) - a withdrawn moq-transport namespace releases its state even when the peer withholds credit
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [moq-c at parity](/quest/m1/c/README.md) - the hand-written `moq-c` ships at the cut with every m1 binding feature moq-ffi gains
- [Lite-05/06 timestamps pass through](/quest/m1/lite-timestamps-through.md) - a timed track received over lite-05/06 stays timed and is forwarded with its timestamps over every version
- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - bindings carry `{value, scale}` timestamps instead of lossy microseconds
- [moq-ffi decoders choose where to start](/quest/m1/ffi-decode-start.md) - moq-ffi decoder outputs take a `start`, so a live player in any binding can skip the cached backlog as moq-c does
- [A broadcast stamps everything from one clock](/quest/m1/broadcast-clock.md) - catalog, MSF, and HLS record groups are stamped from the broadcast's clock, never the ambient one
- [Announcement takeover](/quest/m1/announce-takeover.md) - a new independent route explicitly continues instances served through its predecessor, with shared Rust/JS semantics and lite-07 wire
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [A tightened SUBSCRIBE keeps the start's budget honest](/quest/m1/start-budget-update.md) - a relay copy records the delay its start was resolved under, so a SUBSCRIBE_UPDATE tightening cannot make later readers skip fillable history
- [Broadcast archives](/quest/m1/archive/README.md) - ANNOUNCE, TRACKS, and a SUBSCRIBE per track record every track under a prefix to any object_store, a host serves a stored broadcast back, and the catalog drops its archive pointer for a per-rendition `hls` field
- [HLS recorder target](/quest/m1/hls-record-target.md) - the moq-cli HLS recorder takes its target duration from the declared `hls` duration
- [Lite-07 padding datagram](/quest/m1/lite07-padding-datagram.md) - lite-07 defines a padding datagram that receivers discard, so browser probes can pad below media
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - waiting on Moxygen to send a registered filter type, then the smoke lane stops hiding its failure
- [Smoke token subject](/quest/m1/smoke-token-subject.md) - condition: grant subject ships; then the release smoke test signs with `--subject`
- [Moxygen pin bump](/quest/m1/moxygen-pin-bump.md) - waiting on moxygen#264, then the TRACK_STATUS patch is dropped
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - every surface added since release has a consumer or is gone, overlapping operations are collapsed, names mirror across Rust, JS, and the bindings, and every break says how to migrate
- [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md) - drafts 14-17 and 19-21 are deleted from Rust, JS, tests, interop lanes, and docs, removing the legacy control-stream adapter and its request limit
- [Our moq-transport publishers refuse SUBSCRIBE_TRACKS](/quest/m1/ietf-tracks-refuse.md) - a peer's listing gets NOT_SUPPORTED, while Rust still lists an IETF peer's tracks upstream for the archive
- [A moq-transport request token is one auth per distinct token](/quest/m1/ietf-token-auth.md) - requests naming the same token share one verification and grant, on any request type, ending with the last such request
- [moq-transport carries no epoch](/quest/m1/ietf-epoch-cut.md) - Rust and JS moq-transport sessions stop negotiating MoQ Epoch, so every moq-transport hop is epochless; the draft stays
- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - moq-quic and both QUIC drivers ship one mirrored API: `send::`/`recv::` namespaces, one error per direction, consuming terminal operations, plain configs, no quinn baggage
- [On-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md) - browser publishers answer IETF FETCH for evicted or uncached groups through the JS ranges request surface
- [Rust cancels an unclaimed fill head when a subscription ends](/quest/m1/ietf-fill-end-cancel.md) - SUBSCRIPTION_ENDED cancels, not finishes, a fill no group was sent for, as in JS
- [SUBSCRIBE honors the properties opt-out](/quest/m1/subscribe-timestamps.md) - the Rust publisher sends untimed objects to a SUBSCRIBE that declines Track Properties, and the Rust subscriber asks for them when it needs timestamps
- [A resumed untimed copy keeps DYNAMIC_GROUPS](/quest/m1/ietf-dynamic-groups-resume.md) - opting out of properties on a draft-22 resume no longer forgets DYNAMIC_GROUPS
- [Rust marks a lost listed group unknown](/quest/m1/ietf-publisher-unknown-range.md) - the Rust IETF publisher sends End of Unknown Range for a listed group it can't serve, after the publisher's open quests land
- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - OBS stops sending the top renditions its grant cannot fund, as the Rust/FFI publishers do, and a shaped uplink through moq-ffi proves the resume
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; a manual browser proof and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Decoding from a track's first packet keeps its start](/quest/m1/opus-preskip.md) - moq-audio drops only OpusHead's pre-skip from a track's first packet, keeping the 80 ms warmup for mid-stream joins
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [The publish controls stay visible without a preview](/quest/m1/publish-controls.md) - `<moq-publish>`'s controls no longer collapse with a live camera and no preview
- [Push-to-talk loses no speech](/quest/m1/push-to-talk/README.md) - consumers hold a disabled rendition, and an unmute encodes at once with the mic still open
- [Always-on encoding](/quest/m1/eager-encode/README.md) - publishers encode while enabled whatever the demand, so the past exists to fetch; on-demand is opt-in, and audio returns to it through a raw backbuffer
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [CI housekeeping](/quest/m1/ci-housekeeping.md) - remove the actionlint queue ignore once a release after 1.7.12 reaches the dev shell
- [crates.io trusts the workflow](/quest/m1/crates-trusted-publisher.md) - maintainer action: every published crate gets a trusted publisher for `release-rs.yml`
- [Rust releases publish through OIDC](/quest/m1/release-rs-oidc.md) - release-plz publishes with trusted publishing and `CARGO_REGISTRY_TOKEN` is revoked
- [moq-clock's tests pass under cargo test](/quest/m1/clock-test-process.md) - `paused_runtimes_own_their_anchors` stops depending on being first in its process
- [The JS stalled-consumer eviction test ages its groups](/quest/m1/js-cache-age-test.md) - the test drives the clock the cache reads, so it can fail
- [moq-transport-22 FETCH cost](/quest/m1/ietf22-fetch-poll-cost.md) - #7042's +12.8% instructions on draft-22 FETCH is fixed or attributed
- [An idle track poll exits cheaply](/quest/m1/idle-poll-exit.md) - a reader's poll on a track with nothing new costs a small constant, not ~3,800 instructions
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason, checked by a final paired run before the cut
- [The browser baseline keeps its page logs](/quest/m1/baseline-page-logs.md) - a failing wasm-baseline cell is diagnosable from its artifact
- [Interop coverage](/quest/m1/interop-coverage/README.md) - the riskiest moq-transport features moq-net exposes get cross-implementation tests over drafts 18 and 22
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - A/A and deliberately slowed Nightly runs set the paired comparison's threshold, so its regression alerts can be trusted
- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - nightly ffprobe checks that advertised timing, sync flags, and bytes match the media served
- [Unbounded groups](/quest/m1/unbounded-groups/README.md) - groups and frames lose their size caps; a group keeps a 32 MB tail, and readers behind it replay through paced flights or refresh
- [Next Object on a cold Rust copy](/quest/m1/next-object-cold-copy.md) - a join after an upstream group report starts at the first frame received, never skipping
- [Lite track end at session death](/quest/m1/lite-track-end.md) - Rust and JS abort an ended lite track whose tail never settled, and agree on what declares the end
- [Legacy lite close drain](/quest/m1/lite-legacy-close-drain.md) - a lite-01..06 graceful close never cuts a final group the subscriber has not read
- [moqsrc max-delay](/quest/m1/gst-max-delay.md) - GStreamer's `moqsrc` takes a `max-delay` property instead of a hard-coded 1 s budget
- [moq-clock moves into kio::clock](/quest/m1/kio-clock.md) - moq-clock and its test macro become `kio::clock` and `#[kio::test]` before the cut publishes them
- [Poll-first async](/quest/m1/poll-first/README.md) - library crates compile no coroutines by default: async wrappers are generated from `poll_*` and gated behind an `async` feature
- [HLS export follows a resolution change](/quest/m1/hls-reinit-same-codec.md) - a decoder-identity change under the same codec string starts a new init and segment tag
- [The last AAC segment advertises its full duration](/quest/m1/hls-final-aac-segment.md) - a TS import's final audio segment's EXTINF matches what decodes
- [A JS reader finishes after a clean close](/quest/m1/js-close-range-finish.md) - a range ending below the cache delivers what's held and then ends
- [fMP4 decode times keep increasing](/quest/m1/fmp4-bframe-decode-time.md) - fragments shorter than a GOP of B-frame video never step `tfdt` backwards
- [A JS relay floors an unresolved copy at its newest group](/quest/m1/js-early-latest.md) - lite-01..05 subscribes without a start don't replay the cache from group 0
- [moq-quic deletes its remaining 0-RTT code](/quest/m1/quic-zero-rtt-leftovers.md) - only the code that drops a 0-RTT packet remains, with a test

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
