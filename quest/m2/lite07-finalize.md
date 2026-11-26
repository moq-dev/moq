# [S] Finalize moq-lite-07

## Goal

When the maintainer says to cut it, moq-lite-07 becomes a published version:
it negotiates as `moq-lite-07`, the `moq-lite-07-wip` identifier is gone, its
draft revision describes the wire as it stands, and the next cut of `main`
into `release` ships it in Rust and JS. A deployment pinned to `release` can
then roll lite-07 out without a wip wire changing under it.

## Plan

Decided 2026-10-05 by the maintainer: finalizing lite-07 is upstream work
with its own quest. Until now nothing upstream owned it, while downstream
rollouts waited on it and the
[mesh condition](/quest/m5/lite07-mesh.md) here waited on moq.pro. This quest
waits on nothing outside the repository; the rollouts and the mesh condition
follow it.

Freeze policy, decided by the maintainer 2026-10-05: "just keep adding stuff
to lite-07 until I say to cut it. quests should target the -wip release."
Every wire quest targets the current wip version, and lite-07 keeps growing
until the maintainer calls the cut. This quest is the mechanical rename and
checks done at that call; the Required list is the work already known to land
first, not a closed freeze set.

A route layer lands in lite-07, decided 2026-10-05 and redesigned 2026-11-15
(below). As shipped by [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987),
relays exchange ROUTE, ROUTE_END, DEAD, and DRAIN, every claim belongs to the
relay that announces it, and the native hop list is gone, with
`Path Base`/`Path Keep` compression in place of `Hop Base`/`Hop Keep`; a claim
from a lite-06 peer or across a scoped grant carries a chain instead, and
moq.pro's lite-07 rollout plans for that wire
([lite07-mesh](/quest/m5/lite07-mesh.md)). Only
Routes and announces (finished in [moq-dev/moq#9202](https://github.com/moq-dev/moq/pull/9202)) gated the cut,
not the later federation/P2P integrations. Decided 2026-10-13: routes now
owns the tier/partition correctness tests, so they run before the cut.
Reaffirmed 2026-11-14 (maintainer, the m2 triage, below). Peer
grants and metadata-only subscriptions do not gate it: neither is clustering
stability. Both are additive and, per the freeze policy, target whichever lite
wip is current when they land (superseded 2026-11-26 for metadata-only
subscriptions, which now gate lite-07; see below). Decided 2026-10-25: no routing identity proof
gates the cut, and lite-07 routing identities stay declared and unverified
(2026-11-02: peer grants are dropped and no key proof is planned). Reversed
2026-11-04 (maintainer, /quest-unblock): a peer proves a route key before the
cut, which landed as lite-07's Route Key Setup Parameter. Reversed again
2026-11-15 by the redesign below, which removed the parameter in
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987). Decided 2026-10-26
(maintainer): receivers do not stamp per-session identities either, since
that splits one origin into several identities across a relay mesh.

Decided 2026-10-08: Restart and AUTH join Required, since both change the
lite-07 wire. Compressed tracks joined on 2026-10-17 and landed
([#5928](https://github.com/moq-dev/moq/pull/5928),
[#6228](https://github.com/moq-dev/moq/pull/6228),
[#6230](https://github.com/moq-dev/moq/pull/6230)). SUBSCRIBE_OK carrying live media time was dropped: no reader
was named, and `set_live` covers it.

The cut:

- Every quest under Required has landed.
- No warm field is on main or in an open PR against lite-07-wip:
  [the warmth port](/quest/m3/cache-routing/select.md) waits until after the
  cut (maintainer, 2026-11-20).
- The identifier becomes `moq-lite-07` in `rs/moq-net` and `js/net`, in the
  draft (whose text already names the rename), and at every site spelling
  the wip ALPN: grep the repository for `moq-lite-07-wip`. A wip peer and a
  final peer refuse each other by ALPN rather than misparse; no
  compatibility shim.
- The draft's lite-07 changelog matches the wire and `just drafts check`
  passes. Fix `drafts/draft-lcurley-moq-lite.md:2380` (the lite-07 changelog
  says `0 is malformed` instead of `0 is GROUP_END`, contradicting lines 2218,
  2245, and 2376; found in the 2026-11-21 full-codebase audit).
- SETUP keeps its stronger rule: #4820 put it under the general Message
  Length cap, which only permits rejection (MAY), while "a receiver MUST
  treat a longer SETUP as a protocol violation" stays. The lite-07 changelog
  names that rejection, not only the cap.

Decided 2026-10-20 (maintainer): the drop set joined Required, because it
changes lite-07's SUBSCRIBE_DROP. That wire work, SUBSCRIBE_SKIP and the
`Update` number on SUBSCRIBE, SUBSCRIBE_UPDATE, and SUBSCRIBE_DROP, landed in
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670); what remains of
the drop set is memory accounting with no wire change, so it no longer gates
the cut. Nothing
else from the auth line gates the cut: its lite-07 wire work, the SETUP Token
parameter, has landed, and the rest is API work that does not block the rename.

The archive line's TRACKS request (a per-broadcast track listing that
replaced SUBSCRIBE_ALL) landed in [#6158](https://github.com/moq-dev/moq/pull/6158)
and [#6167](https://github.com/moq-dev/moq/pull/6167), and the padding stream
type for the browser [padding probes](/quest/m5/probe-padding.md) landed in
[#6233](https://github.com/moq-dev/moq/pull/6233).
Decided 2026-10-30 (maintainer, on
[moq-dev/moq#7390](https://github.com/moq-dev/moq/pull/7390)): those probes
pad with datagrams rather than streams, and padding datagrams are supported
only on lite-07. Lite-07 now defines the padding datagram type and both receivers
discard it.
The `Time(pts)` range end rides in the subscribe-ranges children already Required.

Decided 2026-10-27 (maintainer): of Subscribe ranges,
only the children that change the lite-07 wire gate the cut:
Lite-07 ranges ([moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872)),
JS ranges ([moq-dev/moq#7696](https://github.com/moq-dev/moq/pull/7696)), and
Lite-07 FETCH is single-group only ([moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616)). The
fill path, fetch as a range, and the start bookkeeping change no wire.

Decided 2026-10-25 (maintainer): addressing groups by timestamp gates the
cut, since it removes group sequence numbers from the lite-07 wire.

Rust's lite-07 varints already carry the full 64 bits the draft specifies,
so no codec work waits on the cut.

Decided 2026-10-13: servers accept finalized lite-07 by default, and
first-party Rust and JS clients prefer it with older-version fallback. Test
the negotiated version against new and old servers and refusal of the wip
identifier. This release adopts the new routing contract end to end.

Public API: the lite-07 version constant and ALPN lose `-wip`. Wire: lite-07
is published; older versions are unchanged.

Timestamp provenance landed in
[#6150](https://github.com/moq-dev/moq/pull/6150), and the 2^61 - 1
timestamp bound in [#6168](https://github.com/moq-dev/moq/pull/6168).

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): Required names every quest that may still change lite-07 bytes.
One exception, decided 2026-11-19 (maintainer, in the coordinating session):
[the warmth port](/quest/m3/cache-routing/select.md) may add bytes to
lite-07 and is not under Required, because the cut does not wait for it. The
cut is made once every Required quest has landed, and the port is judged
then, by the cut step above: its wire is in lite-07 if the port has landed
and passed the mesh replay by that moment, and otherwise moves to the next
wip version. A port that lands in the days between the last Required quest
and the cut is in, on the same test.
Superseded 2026-11-20 (maintainer, in the coordinating session): the port
waits until after the cut, behind latency link costs on the deployment, so
lite-07 freezes without a warm wire.
The explicit `Oldest` anchor rides [time addressing's model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md),
whose lite-07 encoding [lite-07 by start](https://github.com/moq-dev/moq/pull/8908) carries.

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net): ✅ the cut waits for the symbolic model of the Route Key Parameter
(rejected: neither proof gating, as the TLA+ models do not; the inductive
routing invariant gating too, which sits behind the mesh model). The signed
transcript freezes at the cut and a flaw in it can only be fixed on the wire.
Ended 2026-11-15 with the Route Key (below).

Decided 2026-11-14 (the m2 triage before the cut, under /quest-afk; the
maintainer picked these on
[moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768)), applying the maintainer's rule that m2 keeps only what
would need a breaking release had it landed right after the cut:

- ✅ [The request loop](https://github.com/moq-dev/moq/pull/8881) moved
  to m3 and no longer gates this: a fixture and a fix with no wire change.
  It returned to m2 on 2026-11-15 and still does not gate this.
- ✅ Route layer acceptance stays
  in Routes and announces, so this
  waited on it until [moq-dev/moq#9202](https://github.com/moq-dev/moq/pull/9202) finished both (maintainer, /quest-complete on
  [moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768); rejected:
  moving it to m3 and fixing late routing flaws in lite-08).
- ✅ The Playwright bump moved to m1 and
  gates [the release cut](/quest/m3/release/cut.md) instead of this: the cut
  PR's interop lane runs with `CI_STRICT` (`.github/workflows/interop.yml`),
  which fails the route key lane on a Chromium below 158, so only the pinned
  toolchain clears it. As an early signal, before the rename, run the route
  key lane (`test/interop/clients/js/route-key.ts`) once with Playwright
  1.65.0 or later from npm, which bundles Chromium 158, and put the result in
  the PR. If the tab fails, stop and report it, since only the wire can fix
  the signed transcript. Rejected: keeping the pin in m2, where the freeze
  waits on nixpkgs as well as the cut; dropping the check, which would find a
  transcript flaw only after the freeze. Ended with the route key lane in
  [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987): the
  Playwright quest was deleted, since no lane needs Chromium 158.
- ✅ The wire half of group drops split out as its own quest, which this
  waited on in place of the public drop signal,
  an API change that doesn't touch the wire. It settled without a
  wire-format change ([moq-dev/moq#8774](https://github.com/moq-dev/moq/pull/8774)):
  both publishers already name a Group Stream the subscriber stopped in
  SUBSCRIBE_DROP, which the draft now states, even after a reliable reset.

Decided 2026-11-15 (maintainer, /quest-plan on the whole route layer): ✅
lite-07 carries the redesigned route layer of
[cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md), and the cut waits
for it (rejected: freezing lite-07 with the path rule and redesigning in
lite-08; freezing lite-07 with no route layer). That redesign removes the
Route Key Parameter, so the 2026-11-10 wait on its symbolic model and the
route key lane's early signal above ended when
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) landed.
That epic finished 2026-11-20 with its last child
([moq-dev/moq#9231](https://github.com/moq-dev/moq/pull/9231)), and the cut's
routing wait moved to claim reach (maintainer, 2026-11-20), which
[moq-dev/moq#9290](https://github.com/moq-dev/moq/pull/9290) finished.

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit, which also
audited the lite-07 draft for what it could lose):

- ✅ YIELDED continuations leave lite-07 for lite-08 ([moq-dev/moq#8965](https://github.com/moq-dev/moq/pull/8965)), where they are designed with takeover (rejected: keep them).
- ✅ The `ietf` extension stays, and rides only moq-transport tracks
  (rejected: as it is, where every frame on a relay link carries a record).
- ✅ The `\` path escape is finished rather than
  removed: moq-transport names a track by a tuple, and the escape is how a
  moq-lite path carries one.
- ✅ Subscriber Min Age stays, reversing the 2026-11-14 pick, and
  gains its reader;
  [moq-dev/moq#8839](https://github.com/moq-dev/moq/pull/8839), which
  removed it, was closed.
- ✅ Kept as they are: datagram tracks, including the lite-05/06 bridge (a
  named consumer wants them); all seven lite versions, which keep
  negotiating with today's behaviour; the hidden `.` path prefix; and the
  route plan of [cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md)
  (rejected: routing as a separately versioned SETUP extension).
- The smaller wire fixes rode [lite-07 by start](https://github.com/moq-dev/moq/pull/8908):
  Max Age as value plus one, no Padding stream or datagram type, a kind in
  the track listing, and drops as spans with no count. GROUP `Frame Start`
  left with YIELDED continuations
  ([moq-dev/moq#8965](https://github.com/moq-dev/moq/pull/8965)).
- Not reopened: AUTH_ERROR's retry codes (2026-11-14) and AUTH_OK's rate
  fields, which wait for lite-08 ([rate grant](/quest/m5/rate-grant.md)).

The padding paragraph above is superseded: lite-07 freezes with no Padding
type, and [the padding probes](/quest/m5/probe-padding.md) bring one back in
the version that ships them.

Decided 2026-11-20 (maintainer, /quest-plan auditing the cache, timestamp,
and switching rules in `rs/moq-net/AGENTS.md` before the cut):

- ✅ The cut waits for groups never overlap, [moq-dev/moq#9264](https://github.com/moq-dev/moq/pull/9264)
  (rejected: after the cut). Group times ([moq-dev/moq#9263](https://github.com/moq-dev/moq/pull/9263))
  puts the strong rule in the draft as a publisher MUST: a group's span,
  [Group Start, End) since 2026-11-21, never overlaps another's. Expiry relies on it,
  and first-party producers keep it ([moq-dev/moq#9317](https://github.com/moq-dev/moq/pull/9317)).
- ✅ The frame cap is not Required, like the warmth port. It landed on
  lite-07-wip in [moq-dev/moq#9402](https://github.com/moq-dev/moq/pull/9402):
  a Range may end at a Position.
- The audit's other gaps change no wire and don't gate the cut: a relay
  asking upstream for a missing group head, the switch's group cap, a
  replaced copy's short group, and expiry by a higher group's lowest
  timestamp. A Min Age join that asks upstream is
  [moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265)'s.

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ this quest fixes the changelog typo at
`drafts/draft-lcurley-moq-lite.md:2380` during the cut.

Decided 2026-11-24 (maintainer, on
[moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371#issuecomment-7146881312)):
✅ land that PR's origin ranking now and have the cut wait for the router to
rank one claim prefix's filings as one tier, since the draft now says copies
of one claim rank together wherever each is filed (rejected: keep the
origin's deepest-filing rule until the router lands; fix the router in that
PR).

Decided 2026-11-24 (maintainer, /quest-plan of the exhaustive `moq-net`
subscribe/fetch audit): ✅ the cut waits on
[Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md)
(rejected: bugfixes only without wire changes; drop). That epic freezes
`Previous Delta` on `GROUP` headers, requires `RESET_STREAM_AT` for opened
Group Streams (superseding the 2026-11-14 `SUBSCRIBE_DROP` note for publisher
and peer resets), removes `FETCH` (`0x3`) and `SUBSCRIBE_SKIP` (superseding the
2026-10-20 `SUBSCRIBE_SKIP` note), allows `Anchor::Position` with `Frame == 0`
and exclusive `End` plus one-shot `SUBSCRIBE` + immediate `FIN`, moves
`Priority` and `Group Order` onto each `Range` and `Retune`, and adds judged
`Max Age` to `SUBSCRIBE_DROP(OLD)`.
Superseded 2026-11-26: no `Previous Delta`; the previous group's `GROUP_END` proves the gap, `SUBSCRIBE_DROP(SKIP)` (`0x3A`) covers the rest (removing `SUBSCRIBE_SKIP` `0x3`), and a drop names a group a plain reset may have lost.

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS and lite-07 audit):
✅ the cut also waits on
[Identity rename and 256-byte cap](/quest/m2/identity-rename-and-cap.md)
(renaming `Instance` to `Identity`, capping it at 256 UTF-8 bytes for public
keys, and hashing it in `moq-hls` URLs; rejected: keeping `Instance`, 1024-byte
cap, raw URLs, no wire cap) and on
[metadata-only subscriptions](/quest/m2/subscribe-meta-wire.md) plus its
[subscriber API](/quest/m2/subscribe-meta.md) (reversing the 2026-11-25
deferral to `m3` / lite-08 because `moq-hls` live and archive/VOD indexing is
an immediate consumer and `moq_subscription` changes the C ABI; rejected:
moving only the wire into `m2`, leaving both in `m3`).

## Required

- [Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md) - GROUP_END and SUBSCRIBE_DROP(SKIP) gap accounting, RESET_STREAM_AT, FETCH removal, per-range Priority and Order, and judged OLD drops freeze into lite-07
- [Rename Instance to Identity and cap it at 256 UTF-8 bytes on lite-07](/quest/m2/identity-rename-and-cap.md) - wire rename and 256-byte UTF-8 ceiling on Identity freeze into lite-07
- [Metadata mode on the lite-07 wire, served](/quest/m2/subscribe-meta-wire.md) - Meta byte on SUBSCRIBE and payload-less FRAME freeze into lite-07
- [Metadata-only subscriptions](/quest/m2/subscribe-meta.md) - Rust, JS, and C/binding metadata subscriber API lands before the cut so the C ABI does not break right after 0.22

## Related

- [Takeover](/quest/m3/group-takeover.md) - a later lite version may let a group stream take over for another group; lite-07 freezes without it (maintainer, 2026-11-13)
- [The moq.pro mesh runs lite-07](/quest/m5/lite07-mesh.md) - the deployment condition that follows this
