# m2: the next breaking release

## Goal

Every change that would need a semver-breaking release if it landed right
after the next cut of `main` into `release`, plus the cut
gate, which the [release cut](/quest/m3/release/cut.md) re-creates here each
cycle. The cut waits on this milestone alone;
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
cut gate, decided the same day; the [release cut](/quest/m3/release/cut.md)
holds its template.

Decided 2026-11-12 (maintainer, /quest-audit before the cut):

- ✅ Left m2, so the cut stops waiting on them: the
  [probe epic](/quest/m3/probe/README.md) to m3 and the routes
  [browser proof](/quest/m5/routes-browser-proof.md) to m5 (each waits on a
  browser release and breaks nothing); the docs-only and additive quests and
  outbound federation to [m1](/quest/m1/README.md); and
  [unbounded groups](/quest/m3/unbounded-groups/README.md) to m3, whose byte
  offset goes on the wire version after lite-07 (rejected for each: staying
  in m2). The padding and JS rung disable quests are parked in m5.
- ✅ Joined m2, each a small break the cut would otherwise freeze: the mesh
  [grant shape](/quest/m2/mesh-grant-shape.md) and
  [key budget](/quest/m2/mesh-device-bounds.md), since `moq_auth::mesh`
  publishes for the first time at this cut;
  [auth key validation](/quest/m2/auth-key-validation.md); the
  RTMPS connect signature;
  [usage's derived settings](/quest/m2/usage-derive-settings.md);
  [first-publish versions](/quest/m2/first-publish-versions.md); and three
  API fences: [mesh enrollment](/quest/m2/api-mesh-enrollment.md),
  [the capture selections](/quest/m2/api-capture-fences.md), and
  [the acknowledgment hook's trait](/quest/m2/api-ack-hook-trait.md). None
  is on the routes chain, which stays the critical path.
- ✅ Under the path rule a route keeps one sticky down-only flag beside its
  path; see the [routes epic](/quest/m2/cluster-routes/README.md).

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
native rung disable moved up from the [m2 probe epic](/quest/m3/probe/README.md),
so Rust, FFI, and OBS publishers recover a stopped rendition in this release;
padding and the browser's rung disable stayed in m2, with only padding's
lite-07 stream type here. Both moved to m3 on 2026-11-06 (maintainer,
/quest-unblock), parked for a rethink of padding.

Added 2026-10-22 (maintainer, clustering audit): three interim fixes for
announce churn on today's hop-list mesh, shipping with the next cut from
`main` (not backported to `release`), so deployments get relief before the
route layer ships; and stats buckets as
tracks, so per-bucket stats stop costing cluster announces.

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

Changed 2026-11-08: the rind epic finished in m1, so the cut ships on rind
with no C crypto; aws-lc-rs is out of the workspace.

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

Decided 2026-11-13 (maintainer, /quest-run): ✅ m2 no longer requires the m1
stream-credit epic (rejected: keep the gate). It was an ordering rule, not a
code dependency, and with takeover moved to m3 the epic changes no wire.

Decided 2026-11-13 (maintainer, /quest-plan from an audit of `main`'s audio):
✅ the [audio playout](/quest/m2/audio-playout/README.md) epic moves here from
m1, with its calibration in m3 (rejected: calibration here too; only the lag
rule here). Its lag rule reshapes `main`-only API the cut would publish (the
`stretch` setting, moq-audio's `Sink::due` and `bus::Positioned`), and the
cut should not ship the 2026-11-09 stretch model, which lets audio trail
video by up to 2 s.
