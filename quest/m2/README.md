# m2: the next breaking release

## Goal

Every change that would need a semver-breaking release if it landed right
after the next cut of `main` into `release`, plus the [cut
gate](/quest/m2/cut-gate/README.md). The cut waits on this milestone alone;
complete it before cutting main into release.

## Plan

Decided 2026-11-05 (maintainer, milestone re-cut): m2 holds the breaking half
of what was "everything before the cut" (2026-10-20). Breaking covers every
published surface, as the root [questline](/quest/README.md) defines: the
public Rust, JS, and bindings API, a frozen wire version, the hang catalog
JSON, a stored or exported container format, and CLI flags, config keys, and
environment variables. A change to lite-07-wip or to a `main`-only API the
cut would publish counts, because it could not land after the cut without
another breaking release. A non-urgent fix that breaks joins m2; an urgent
one joins m0. Placed in the re-cut: group-drops (its shape is open, but
naming abandoned groups on the wire lands on lite-07-wip) and the whole
unbounded-groups epic, whose additive compressed-stream child rides with it.

The cut gates that break nothing (main no slower than release, vendored
fixes, and moq-c parity) live under the recurring
[cut gate](/quest/m2/cut-gate/README.md), decided the same day.

Held by open claims: the m1 epics and quests marked below stay where they
are until their claims merge, and the [probe epic](/quest/m2/probe/README.md)
stays here while its open claims last, so the cut waits on it until the
[re-cut follow-up](/quest/m2/recut-held.md) moves it to m3. Its padding and JS
rung disable quests already moved to m3 on 2026-11-06.

Carried from m1's Plan before the re-cut, where "m1" means the whole pre-cut
milestone, now m1 and m2:

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
  the cut under [release execution](/quest/m3/release/README.md).

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
padding and the browser's rung disable stayed in m2, with only padding's
lite-07 stream type here. Both moved to m3 on 2026-11-06 (maintainer,
/quest-unblock), parked for a rethink of padding.

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

Added 2026-11-04 (maintainer): the relay reports the native sessions its
drain deadline force-closes while still open, apart from those only finishing
their end report, so an embedder's drain alert pages on real cuts.

Added 2026-11-07 (maintainer, /quest-plan from an API shape audit of
`release` against `main` and the open quests): the breaks it found that the
cut would otherwise publish. A route's role becomes its type; JS durations
and instants become objects; native playout sync decouples its tracks like
the browser's; sentinels, moq-video's environment reads, and per-gateway
ingest setters go; and an epic batches the small fixes per area.

Added 2026-11-07 (maintainer, /quest-plan from an API sweep of the open PRs
and the remaining quests): the shapes later quests would otherwise break
after the cut. Public spawning constructors, moq-audio's codec seam,
consuming `finish`, the new `bus` signatures, the moq-auth iroh variant, and
the `warmup` contract settle here; growing structs are fenced with
`#[non_exhaustive]` through the api-shape epic.

Decided 2026-11-05 (maintainer, /quest-audit): blockers rank above the
quests that require them.

- ✅ Move the blockers up: Drop set and Subscribe ranges rank just above
  Address groups by timestamp, and the API audit just above Cluster routing.
- Not chosen: moving the dependents down; leaving the order as it was.

## Required

- [Drop set](/quest/m1/drop-set.md) - the drop set's track and cursor runs are billed to the cache pool, so fragmenting drops costs what the pool holds; eviction folds final runs or ends the cursor with `TooFarBehind` (held in m1 by an open claim)
- [Subscribe ranges](/quest/m2/subscribe-ranges/README.md) - a lite-07 SUBSCRIBE asks for past and live ranges with absolute or relative ends, in either order, and replaces FETCH for multi-group reads (single-group FETCH stays); merged ranges never starve a subscriber; relays fill misses by range, including over moq-transport
- [Address groups by timestamp](/quest/m2/time-addressing/README.md) - group start timestamps replace sequence numbers in the moq-net API and lite-07 wire, while old subscribers, old publishers, and published JS clients keep working
- [Cluster routing](/quest/m2/cluster-routing/README.md) - prove shared routing contracts, implement per-origin routes, and connect outbound local clusters
- [A job's exact route continues its claim's readers](/quest/m2/announce-takeover.md) - a job's route inherits its claim's instance, replacing the ANNOUNCE_TAKEOVER predecessor design
- [A route's role is its type](/quest/m2/route-roles.md) - no route method panics, throws, or errors because the other constructor made the route; prototypes of a single prefix route and an exact/prefix split decide the shape
- [A group a subscriber gives up on is always reported](/quest/m2/group-drops.md) - the wire names every abandoned group and the drop signal is public, so readers count losses instead of stalling
- [Stream credit by priority](/quest/m1/stream-credit/README.md) - every stream opens through one priority entry point, and a higher-priority group takes over or resets a lower one when credit runs out (held in m1 by an open claim)
- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - when the maintainer cuts it, lite-07 negotiates as `moq-lite-07` and the next release ships it
- [Unbounded groups](/quest/m2/unbounded-groups/README.md) - groups and frames lose their size caps; a group keeps a 32 MB tail, and readers behind it replay through paced flights or refresh
- [moq-quic drops inherited surfaces with no consumer](/quest/m2/moq-quic-unused-surface.md) - unused accept, connection, and stream-id surfaces leave moq-quic's public API before the cut
- [Publish reports rendition stats as hang's type](/quest/m2/rendition-stats-type.md) - `Rendition.stats` and `Report.Writer` take the rendition stats schema's input type, so encoders write plain numbers
- [No sentinel values in public API](/quest/m2/no-sentinels.md) - receive windows, cache durations, reconnect timeouts, and the FFI test clock use `Option` instead of a huge value or zero
- [moq-video takes devices, not environment variables](/quest/m2/video-devices.md) - VA-API and V4L2 device paths are config fields, and `moq_video::ENV` never ships
- [Gateways share one ingest config and one rate cap](/quest/m2/gateway-ingest.md) - `moq_mux::Ingest` replaces 18 per-gateway setters, and the live rate cap moves to `moq_net::bandwidth`
- [Native playout sync keeps tracks decoupled](/quest/m2/mux-sync.md) - `moq_mux::timeline` becomes `moq_mux::sync`, and one slow track no longer stalls or moves another
- [JS time values are types, not numbers](/quest/m2/js-time-types.md) - every public JS duration and instant is a `Clock.Duration` or `Clock.Instant` object, so a unit mistake fails to compile
- [Small API fixes before the cut](/quest/m2/api-shape/README.md) - one path and one spelling per item, names by role, and no dead or wrong-role public surface across net, transport, media, native, apps, JS, and bindings
- [Public constructors spawn nothing, or say why](/quest/m2/spawn-constructors.md) - each public library entry point that spawns returns a driver its caller polls, or is listed as kept
- [moq-audio codecs return zero or more outputs](/quest/m2/audio-codec-seam.md) - `encode` and `decode` return any number of outputs and gain `flush`, as moq-video's do
- [Bus writes land at a bus position](/quest/m2/bus-positioned-write.md) - callers sync audio to video without copying the bus's private latency, before the cut first publishes `bus`
- [Drop moq-auth's iroh transport](/quest/m2/auth-drop-iroh.md) - this breaking moq-auth release drops `Transport::Iroh` and the JS `"iroh"` value
- [Plan: recovery boundaries](/quest/m2/intra-refresh-boundary.md) - the catalog's new `warmup` contract is settled before the cut publishes it
- [MoQ on rind](/quest/m1/rind/README.md) - every MoQ crate takes its crypto from rind, a pure-Rust graviola fork, so no build compiles C crypto (held in m1 by an open claim)
- [moq auth serve bounds what a leaked token or public caller can do](/quest/m2/auth-serve-policy.md) - tokens need an `exp` unless the operator opts out, and a public listener authenticates relays
- [Finishing a producer returns a Finished handle](/quest/m2/finished-handle.md) - `finish(self)` consumes moq-net's and the media crates' producers, so appending after finish doesn't compile
- [Cut readiness](/quest/m2/cut-gate/README.md) - the checks the cut needs that break nothing: main no slower than release, vendored fixes ported, and moq-c at parity
- [Probe](/quest/m2/probe/README.md) - a session probes up to the bitrate it wants to send or receive, so a stopped simulcast rendition resumes and a viewer up-switches; browser padding is parked in m3 (held in m2 by open claims; moves to m3)
- [Finish the milestone re-cut](/quest/m2/recut-held.md) - claim-held epics move to the milestone their kind and priority give them once their claims merge
