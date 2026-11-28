# m2: the next breaking release

## Goal

Every change that would need a semver-breaking release if it landed right
after the next cut of `main` into `release`, and the non-breaking work the
cut or [lite-07 finalization](/quest/m2/lite07-finalize.md) requires. A quest
stays here only for a reason of its own: the cut waiting on m2 is not one. The
[release cut](/quest/m3/release/cut.md) and its pre-cut reviews wait on this
milestone (plus `cut.md`'s m0 blockers, not m1); complete it before cutting
main into release.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit of 100 recently merged PRs):
- ✅ Added six pre-0.22 wire, public-API, and behavior quests to `m2`: [routing every lite-07 SUBSCRIBE through TrackRun](/quest/m2/lite07-single-subscribe-engine.md) (deleting `#9687`'s `OneShotFetch` race), [lite-07 timestamp mismatch egress and proven ledger bounds](/quest/m2/lite07-timestamp-mismatch-and-ledger.md) (`#9794`, `#9686`), [clean unwanted drop in moq-json](/quest/m2/moq-json-unwanted-drop.md) (`#9550`), [unifying moq_mux::catalog consumers on Timed and hiding hang::Container audio variants](/quest/m2/catalog-timed-consumers-and-container.md) (`#9714`, `#9553`), pre-cut API cleanup ([moq-dev/moq#9874](https://github.com/moq-dev/moq/pull/9874)) (`#9832` private `cluster::Shared`, `#9835` `Result` constructors in `moq-stats`, `#9518` hidden `poll_quiesce`), and [KeepAlive API reconciliation and PING-only probes](/quest/m2/keep-alive-probes-and-api.md) (`#9698`); confirmed [moq-dev/moq#9793](https://github.com/moq-dev/moq/pull/9793)'s `broadcast.meta()` API stays as-is; verified [moq-dev/moq#9719](https://github.com/moq-dev/moq/pull/9719) fixed `#9607`'s stream-credit shedding vs age-expiry `OLD` drop bug; and pulled [hls-group-end-timing](/quest/m2/hls-group-end-timing.md) and [front-drop-with-next-group](/quest/m2/front-drop-with-next-group.md) back from `m3` (reversing `#9842` for those two).

Decided 2026-11-28 (maintainer, /quest-run): ✅ until the `0.22.0` cut, m0
to m2 hold only what the cut or lite-07 finalization needs, plus
semver-breaking API or wire changes; every other quest moved to m3 and ships
later in a patch version (rejected: moving only m1; keeping all of m0 as an
ops bucket). The [questline](/quest/README.md) states the rule.

Here, [hls-group-end-timing](/quest/m2/hls-group-end-timing.md) moved: it
rechecks HLS timing after lite-07 is finalized and changes no API (superseded
2026-11-28: pulled back into m2, above). The
typed superseded group end (additive) stays only while its PR claims it.

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Added [key groups by frame 0 and remove group timestamps from producer APIs](/quest/m2/group-frame-timestamps.md)
  to `m2` gating [lite07-finalize](/quest/m2/lite07-finalize.md) after
  timestamp-ids ([moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794)) (rejected: folding into
  [moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794); dropping).
  Creates groups eagerly from frame 0 across Rust, JS, `moq-ffi`, `libmoq`, and
  bindings, removes `seek(start)`, exposes `drop_groups` in Rust and JS, omits
  lite-07's first `FRAME` delta at `Frame Start = 0`, removes zigzag from
  lite-07, and keeps `GROUP_END` as the last frame's exclusive end.
- ✅ Added [delete `moq-pattern` and `@moq/pattern`](/quest/m2/delete-moq-pattern.md)
  and removing `identity` from `hang::catalog` (landed)
  to `m2` before the `0.22.0` cut (rejected: defer either or both to `m3`;
  keep `moq-pattern` or catalog `identity`). Wildcards never transit the wire,
  grants already moved to `Prefixes`, and wire announcements already carry
  route `Identity` (preserving `moq_net::Identity` and
  `BroadcastConsumer::info().identity`), so neither should freeze into `0.22.0`.

Decided 2026-11-26 (maintainer, pre-cut quest and consumer audit):

- ✅ Added pre-cut Rust API fences (`#[non_exhaustive]`
  on `moq_audio::playback::Device` for `moq-dev/studio` m0 and
  `moq_audio::decode::Read` for `audio-playout-library`, plus exempting
  `moq-opus` from the shared `0.22.0` minor while `opus-safe` replaces its
  c2rust surface; rejected: also fencing `App::source`, `Applied::cut`, and
  `error-matches` `Ok(None)` before `0.22.0`; leaving all fences for the next
  API cut).
- ✅ Moved three non-breaking quests out of m2 so they stop stalling m2 and the
  five pre-cut release reviews: [TS export audio rate](/quest/m1/ts-export-audio-rate.md)
  and [TS unpadded audio buffer](/quest/m3/ts-unpadded-audio-buffer.md) to m1 (moved on to m3
  2026-11-28),
  and kio's crates.io publish (since dropped: kio moved back into `rs/kio`) to m0 as a direct
  `Required` gate on [cut.md](/quest/m3/release/cut.md) (rejected: keeping them
  in m2).

Decided 2026-11-21 (maintainer, /quest-plan of the main → release routing
audit): the cut also waits on the routing fixes that audit found. NodeId
drops Start, whether an origin forwards moves into its config, and moq-relay's
cluster peers lose `egress`.
That audit's three m1 fixes (the scoped peer, the route floors, and the
origin's route rank) have landed, and so has the lite-07 router's half of the
rank.

Decided 2026-11-21 (maintainer, /quest-plan of an exhaustive full-codebase
audit before the `0.22.0` cut, covering both pre-release and new code): the cut
also waited on the breaking fixes that audit surfaced across the wire, vendored
native crates, and public APIs, and all of them have landed.

Non-breaking findings from the same audit live in
[m0](/quest/m0/README.md) (release-cut pipeline and packaging blockers),
[m3](/quest/m3/README.md) (softer edge cases and low-severity cleanups), and
[m4](/quest/m4/fuzz-binary-parsers.md) (binary-parser fuzz targets).

Decided 2026-11-23 (maintainer): ✅ the cut waits for a
safe on-demand drop and deleting `abort_unwanted`, so 0.22 never
publishes an API the next release removes (rejected: leaving them in m3,
cutting first and deleting in the next breaking release).

Decided 2026-11-24 (maintainer, /quest-plan of the exhaustive `moq-net`
subscribe/fetch audit): ✅ the cut waits on
the lite-07 subscribe and fetch redesign (finished with [moq-dev/moq#9719](https://github.com/moq-dev/moq/pull/9719)),
which adds `Previous Delta` to `GROUP` headers, requires `RESET_STREAM_AT` for
opened Group Streams, removes `FETCH` and `SUBSCRIBE_SKIP` on lite-07, moves
`Priority` and `Group Order` onto per-range wire fields and public
`track::Range` across Rust, JS, and all FFI bindings, and adds judged `Max Age`
to `SUBSCRIBE_DROP(OLD)` (rejected: bugfixes only without wire changes; drop).
This supersedes `quest/m2/time-addressing/per-range-start.md` (#9491) and
#9538; the non-wire progress and cache-invariant fixes landed separately in
moq-dev/moq#9564.
Superseded 2026-11-26: no `Previous Delta`; the previous group's `GROUP_END` proves the gap, `SUBSCRIBE_DROP(SKIP)` (`0x3A`) covers the rest (removing `SUBSCRIBE_SKIP` `0x3`), and a drop names a group a plain reset may have lost.

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS and lite-07 audit):
✅ the cut also waits on
the Identity rename and 256-byte cap ([moq-dev/moq#9720](https://github.com/moq-dev/moq/pull/9720))
(renaming `Instance` to `Identity` across every wire message, capping it at
256 UTF-8 bytes for public keys, and hashing it in `moq-hls` URLs; rejected:
keeping `Instance`, 1024-byte cap, raw URLs, no wire cap) and on both
metadata mode on the lite-07 wire, served ([moq-dev/moq#9758](https://github.com/moq-dev/moq/pull/9758)),
and its subscriber API ([moq-dev/moq#9793](https://github.com/moq-dev/moq/pull/9793)) (reversing the
2026-11-25 deferral to `m3` / lite-08 because `moq-hls` live and archive/VOD
indexing is an immediate consumer and `moq_subscription` changes the C ABI;
rejected: moving only the wire into `m2`, leaving both in `m3`).

Decided 2026-11-16 (maintainer, /quest-plan of an audit of everything on
`main` since `release`, and of the lite-07 draft): the cut also waits on what
that audit found. Fifteen reviewers judged about 480 work lines for a
long-term purpose and a caller, against moq.pro's and Kixel's roadmaps; the
maintainer decided each finding in a 15-round interview, and each quest's
Plan records its decision with what was rejected. The audit reopened the 50
work lines the 2026-11-14 [feature review](/quest/m3/release/feature-review.md)
judged "ship as is". Everything below is breaking by this milestone's rule: a
removal, a rename, or a wire, catalog, or format change that is free until
0.22 publishes it.

- **lite-07 wire.** YIELDED continuations left for lite-08
  ([moq-dev/moq#8965](https://github.com/moq-dev/moq/pull/8965)); the ietf extension
  costs nothing on a moq-lite track; the path escape
  is finished; Subscriber Min Age stays and
  gains its reader, reversing 2026-11-14; and
  [lite-07 by start](https://github.com/moq-dev/moq/pull/8908) carries the
  smaller wire fixes. Refresh ([moq-dev/moq#9054](https://github.com/moq-dev/moq/pull/9054)) and
  group drops took simpler contracts.
  [moq-transport time addressing](https://github.com/moq-dev/moq/pull/8918)
  returned from m4, and the two moq-transport interop quests in m3 were
  deleted. Kept as they are: datagram tracks with their lite-05/06 bridge,
  all seven lite versions, the hidden `.` path prefix, and the route plan
  of [cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md), whose switch
  settled the public route API in
  [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053). The
  moq-cluster draft stays, since third-party relays ship it, rewritten to
  what the binding sends.
- **moq-net API.** [Socket endpoints](https://github.com/moq-dev/moq/pull/9029) leave for Kixel, and the
  demos return to their earlier format.
- **Unused API.** One sweep per area (done: the last,
  [moq-dev/moq#9196](https://github.com/moq-dev/moq/pull/9196)), each item
  re-checked for a downstream caller first.
- **Auth and crypto.** End-to-end encryption is its own unpublished crate again
  ([moq-dev/moq#8947](https://github.com/moq-dev/moq/pull/8947)); its
  [finished format](/quest/m3/e2ee-format.md) follows in m3.
- **Catalog, drafts, and formats.** hang-04 (done: the `sync` section, a version per entry, no video `warmup`, the TS passthrough under
  `mpegts`, one strict rule);
  declared ends, back from m3, which became a whole marker on 2026-11-19
  and an end again on 2026-11-20 ([moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263)).
- **Native media and gateways.** [Upstream str0m](https://github.com/moq-dev/moq/pull/8963), which reverses the
  2026-10-30 vendoring.
- **Bindings.** The C API as `libmoq` (done), reversing 2026-10-25.
  The bindings break twice by decision: at 0.22, and again at
  [the exporter switch](/quest/m3/ffi-exporter.md).

Two rules changed with it (AGENTS.md): a feature reaches the bindings when a
binding user needs it, and a path segment's leading `.` is the one marker
allowed inside a value. Not reopened: AUTH_ERROR's retry codes (2026-11-14)
and AUTH_OK's rate fields, which wait for lite-08
([rate grant](/quest/m5/rate-grant.md)).

Decided 2026-11-20 (maintainer, /quest-plan of a pre-cut audit of the
lite-07 and hang-04 wire changes): the cut also waits on what that audit
found. Each fix is so that a subscription, frame, or catalog entry means
the same whichever relay or reader serves it:
no relative anchors ([moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265)),
with Min Age placing a live join;
scale-less tracks aging by arrival ([moq-dev/moq#9266](https://github.com/moq-dev/moq/pull/9266));
Group Streams carrying each time once ([moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263)),
with GROUP_END's end back as a delta;
time anchors landing by group spans ([moq-dev/moq#9406](https://github.com/moq-dev/moq/pull/9406));
and first-party producers keeping those spans apart ([moq-dev/moq#9317](https://github.com/moq-dev/moq/pull/9317)).
[moq-dev/moq#9198](https://github.com/moq-dev/moq/pull/9198) and
[moq-dev/moq#9221](https://github.com/moq-dev/moq/pull/9221) closed, since
relative starts and the bare GROUP_END are gone.

Decided 2026-11-15 (maintainer, /quest-plan on the whole route layer): ✅ the
cut waits on [cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md), which
replaces lite-07's route layer before it freezes (rejected: lite-08). The
routes chain stays the critical path, now through that epic.
That epic finished 2026-11-20
([moq-dev/moq#9231](https://github.com/moq-dev/moq/pull/9231)), and the cut's
routing wait moved to claim reach, which
[moq-dev/moq#9290](https://github.com/moq-dev/moq/pull/9290) finished.

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
  browser proof to m5, since deleted with the route key (each waits on a
  browser release and breaks nothing); the docs-only and additive quests and
  outbound federation to [m1](/quest/m1/README.md); and
  [unbounded groups](/quest/m3/unbounded-groups/README.md) to m3, whose byte
  offset goes on the wire version after lite-07 (rejected for each: staying
  in m2). The padding and JS rung disable quests are parked in m5.
- ✅ Joined m2, each a small break the cut would otherwise freeze: the mesh
  grant shape and key budget, since `moq_auth::mesh`
  publishes for the first time at this cut (both dropped 2026-11-14, when
  `moq_auth::mesh` left moq for Kixel);
  auth key validation; the
  RTMPS connect signature;
  usage's derived settings; and two
  API fences: mesh enrollment (dropped with it) and
  the capture selections. None
  is on the routes chain, which stays the critical path.
- ✅ Under the path rule a route keeps one sticky down-only flag beside its
  path; see the routes epic, finished in
  [moq-dev/moq#9202](https://github.com/moq-dev/moq/pull/9202). The
  route layer that replaced it keeps the flag as a route's mark
  ([cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md) item 6).

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
✅ the audio playout epic moves here from
m1, with its calibration in m3 (rejected: calibration here too; only the lag
rule here). Its lag rule reshapes `main`-only API the cut would publish (the
`stretch` setting, moq-audio's `Sink::due` and `bus::Positioned`), and the
cut should not ship the 2026-11-09 stretch model, which lets audio trail
video by up to 2 s.

Decided 2026-11-14 (m2 triage before the cut, under /quest-afk; the
maintainer asked that what remains be unblocked or punted, and picked these on
[moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768)). m2 keeps only
what would need a breaking release had it landed right after the cut:

- ✅ Left m2: [the request loop](https://github.com/moq-dev/moq/pull/8881)
  to m3's cluster-routing epic (a fixture and a fix, no wire), until it
  returned to m2 on 2026-11-15 (maintainer, on
  [moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832)) and became
  the request quest of [cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md);
  media consumers (later start placement) to m3 (its API
  breaks ride the model; dropping hang's markers is tolerated by readers),
  until it returned to m2 on 2026-11-16;
  [moq-transport time addressing](https://github.com/moq-dev/moq/pull/8918) to m4
  (low-priority compatibility, no named consumer), until it returned to m2
  on 2026-11-16; and the
  Playwright bump to m1, which the release cut required in place of
  lite07-finalize (its strict interop lane needed Chromium 158), with
  lite07-finalize checking Chromium 158 once by hand; deleted with the route
  key lane it served. The E2EE nonce
  quest was removed: the nonce already binds the wire ID, which becomes the
  start, and lite-07 by start rewords the draft. Rejected for each: staying
  in m2.
- ✅ Stayed in m2 though not breaking itself:
  [the interest set](https://github.com/moq-dev/moq/pull/8728), because
  [the route stream](https://github.com/moq-dev/moq/pull/8835) needs it.
  The route stream stays because lite-07 would otherwise freeze with one
  router key per announce stream, which the route model doesn't check, and
  keep the reopen-and-hand-over machinery for its lifetime (rejected: moving
  both to lite-08).
- ✅ Route acceptance stays in
  the routes epic and gates the cut (both finished in
  [moq-dev/moq#9202](https://github.com/moq-dev/moq/pull/9202)), per the 2026-10-13 rule that routes'
  tier and partition tests run before it (maintainer; rejected: moving it to
  m3 and fixing late routing flaws in lite-08).
- ✅ Group drops split: its lite-07 wire question
  was its own quest, settled without a wire-format change in
  [moq-dev/moq#8774](https://github.com/moq-dev/moq/pull/8774); the public
  `Ordered` API waits for the model.
  Catalog at timestamp no longer waits
  for `@moq/net` by start, and the bindings shrank to a rename, since the model
  already filled each field with the start.

Decided 2026-11-17 (maintainer, on
[moq-dev/moq#8998](https://github.com/moq-dev/moq/pull/8998#issuecomment-6947837677)):
✅ rising background noise joins m2 though it breaks nothing. It ports libopus's analysis step, so the browser voice gate
that PR ships suppresses noise starting mid-call as the Rust publisher's
libopus does (rejected: a faster SILK floor rise; dropping it).
