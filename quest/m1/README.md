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
  claim continuity on lite-07. Catalogs, long-lived readers,
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

Added 2026-11-01 (maintainer): datagram tracks, so lite-07 freezes with
datagrams as their own track kind instead of single-frame groups.

Added 2026-11-01 (maintainer, follow-up planning): byte-stream ladders, so
the binding API for `lower` breaks before the cut, `moq export` ending
with its broadcast, and a relay answering a SUBSCRIBE on a fetched copy.

Added 2026-10-27 (maintainer, from an external release-to-main audit): the
bindings' silent `max_delay` default change and the upgrade notes that
contradict the code. Its other findings already had owners or contradicted
settled decisions (archives break in place; OBS video-only output stays
removed).

Added 2026-11-01 (maintainer, follow-ups from the 2026-11-01 /quest-afk run):
the shared pacer's overshoot under 2 Mbit/s with the BBR3 retune it needs,
since BBR3 is the default controller the cut ships; a truncated MKV that
left its tracks open; and, under release performance, a bisect of the relay's
per-frame CPU growth since #5926.

## Required

- [Address groups by timestamp](/quest/m1/time-addressing/README.md) - group start timestamps replace sequence numbers in the moq-net API and lite-07 wire, while old subscribers, old publishers, and published JS clients keep working
- [Pre-cut audit findings](/quest/m1/cut-audit/README.md) - fix the crashes, hostile-input costs, and correctness bugs the 2026-10-24 audit of main found
- [Vendored code has every upstream fix it needs](/quest/m1/vendor-sync/README.md) - quinn, libv4l-rs, nvidia-video-codec-sdk, rust-media-libs, cros-codecs, and UniFFI generator fixes are ported or planned, once per release cycle
- [Media fuzz findings](/quest/m1/fuzz-findings/README.md) - the nightly media fuzz targets run clean: mp4-atom, MKV block, TS step, SPS overflow, and catalog encoder bugs they found are fixed
- [Cluster routing](/quest/m1/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [moq-c at parity](/quest/m1/c/README.md) - the hand-written `moq-c` ships at the cut with every m1 binding feature moq-ffi gains
- [JS claims through a relay on lite-07](/quest/m1/js-claims/README.md) - a claimed path resolves through `origin.request()`, survives re-serving, and reaches a replaced track, found by Kixel's T3 host
- [A job's exact route continues its claim's readers](/quest/m1/announce-takeover.md) - a job's route inherits its claim's instance, replacing the ANNOUNCE_TAKEOVER predecessor design
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH; merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Datagram tracks](/quest/m1/datagram-tracks/README.md) - a track is datagrams or streams; datagram tracks have their own request and sequence, no ranges, no cache, and map to object ID 0 on moq-transport
- [A step back smaller than the delay keeps the TS export](/quest/m1/ts-small-step-back.md) - audio restarts instead of regressing; video stays within its declared depth
- [Taking control of a CARLA truck starts from neutral pedals](/quest/m1/carla-takeover-neutral.md) - a viewer's takeover releases the autopilot's throttle instead of driving off the road on it
- [A wedged CARLA truck nobody drives respawns after the stuck timeout](/quest/m1/carla-stuck-handback.md) - find what kept the 300 s stuck respawn off for a truck wedged after a handback, and fix it
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [Sockets](/quest/m1/socket/README.md) - a generic dial/accept `Socket` in moq-net, Rust, JS, and Python, with tracks as streams; moq-boy and CARLA move onto it
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - every surface added since release has a consumer or is gone, overlapping operations are collapsed, names mirror across Rust, JS, and the bindings, and every break says how to migrate
- [Register moq-dev draft 20 in the interop runner](/quest/m1/ietf-drop/README.md) - land the external registry, notes, and README correction for drafts 18, 20, and 22
- [moq-quic's API improves on quinn's](/quest/m1/quic-api/README.md) - moq-quic and both QUIC drivers ship one mirrored API: `send::`/`recv::` namespaces, one error per direction, consuming terminal operations, plain configs, no quinn baggage
- [On-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md) - browser publishers answer IETF FETCH for evicted or uncached groups through the JS ranges request surface
- [Native rung disable proof](/quest/m1/simulcast-rung-disable.md) - a shaped uplink through moq-ffi proves the top rendition stops and resumes over real BBR3 probing
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; time-stretched browser playback without steady-state underruns and a native trace replay remain
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [Push-to-talk loses no speech](/quest/m1/push-to-talk/README.md) - consumers hold a disabled rendition, and an unmute encodes at once with the mic still open; a reordered head after a long mute is still lost at the listener
- [Push-to-talk's press-to-audible latency is chosen, not inherited](/quest/m1/push-to-talk-latency.md) - each part of an ideal-path press's 469 ms, chiefly the 300 ms delay and 115 ms device output, is reduced through its knob or kept with a reason
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [CI housekeeping](/quest/m1/ci-housekeeping.md) - remove the actionlint queue ignore once a release after 1.7.12 reaches the dev shell
- [crates.io trusts the workflow](/quest/m1/crates-trusted-publisher.md) - maintainer action: every published crate gets a trusted publisher for `release-rs.yml`
- [Rust releases publish through OIDC](/quest/m1/release-rs-oidc.md) - release-plz publishes with trusted publishing and `CARGO_REGISTRY_TOKEN` is revoked
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason, checked by a final paired run before the cut
- [Interop coverage](/quest/m1/interop-coverage/README.md) - the riskiest moq-transport features moq-net exposes get cross-implementation tests over drafts 18, 20, and 22
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - A/A and deliberately slowed Nightly runs set the paired comparison's threshold, so its regression alerts can be trusted
- [Unbounded groups](/quest/m1/unbounded-groups/README.md) - groups and frames lose their size caps; a group keeps a 32 MB tail, and readers behind it replay through paced flights or refresh
- [Poll-first async](/quest/m1/poll-first/README.md) - library crates compile no coroutines by default: async wrappers are generated from `poll_*` and gated behind an `async` feature
- [Authored tracks default to a 30 s max-age](/quest/m1/max-age-defaults.md) - raw and media tracks advertise 30 s unless their author chooses otherwise, with explicit unlimited in every binding
- [HLS export lists a same-codec reconfigure's first group under the new init](/quest/m1/hls-reinit-same-codec.md) - the hand-over pick, whichever of the catalog and the records arrives first, after the catalog `hls` removal is planned
- [Ordered reads can skip owed holes, and snapshot latest() does](/quest/m1/ordered-owed-holes.md) - moq-net's `Ordered` gains a read that skips past an owed hole to the newest buffered group, while keeping the reader's bounds
- [A Rust relay never replays from group 0 to a lite-03/04 subscriber](/quest/m1/relay-inflight-replay.md) - A Rust relay or publisher serving a lite-03 or lite-04 SUBSCRIBE from a track copy that is still resolving starts at the newest group once the copy resolves
- [The resume model ends #7739's clean-end seed](/quest/m1/resume-seed-17893651.md) - replay seed 17893651 after #7705's wake fix, and fix it if a long-budget reader still never gets the end
- [Every moq-mux consumer gets the catalog warmup from one entry point](/quest/m1/mux-warmup.md) - one moq-mux call widens the subscription and configures the consumer, adopted by moq-audio, moq-c, moq-ffi's raw consumer, and moqsrc
- [Codex shells never load another checkout's dev environment](/quest/m1/codex-env-snapshot.md) - the Codex dev-shell snapshot is skipped when stale or inside `nix develop`
- [Rename moq-rtc to moq-webrtc](/quest/m1/webrtc-rename.md) - one published `moq-webrtc` crate holds the gateway, the ICE driver, and the data-channel transport
- [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md) - str0m, dimpl, `is`, and sctp-proto live in moq-webrtc with unused code removed, so moq-rtc can take rind
- [MoQ on rind](/quest/m1/rind/README.md) - every MoQ crate takes its crypto from rind, a pure-Rust graviola fork, so no build compiles C crypto
- [A hardware decoder's capability refusal is an unsupported config](/quest/m1/hw-decoder-refusal.md) - a GPU that can't decode a stream reports `UnsupportedConfig`, so callers fall back to another rendition
- [A JS audio capture gap keeps the audio before it](/quest/m1/js-audio-gap-flush.md) - every capture gap flushes the `AudioEncoder` before restarting it, like a disable
- [A draft-18 resume of a cached group under a finite max-age settles](/quest/m1/ietf18-resume-cached-start.md) - the read delivers the group or refuses at once, never hangs
- [Bot reviewers don't re-review every push](/quest/m1/review-bot-waves.md) - CodeRabbit and Pullfrog review on ready and on request, so fixes stop drawing new thread waves

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
