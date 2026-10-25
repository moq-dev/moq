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
[mesh condition](/quest/m3/lite07-mesh.md) here waited on moq.pro. This quest
waits on nothing outside the repository; the rollouts and the mesh condition
follow it.

Freeze policy, decided by the maintainer 2026-10-05: "just keep adding stuff
to lite-07 until I say to cut it. quests should target the -wip release."
Every wire quest targets the current wip version, and lite-07 keeps growing
until the maintainer calls the cut. This quest is the mechanical rename and
checks done at that call; the Required list is the work already known to land
first, not a closed freeze set.

The route layer of [Cluster routing](/quest/m1/cluster-routing/README.md)
(ROUTE_START/UPDATE/END, path-less ANNOUNCE, the hop list dropped) lands in
lite-07, decided 2026-10-05. That removes lite-07's `Hop Base`/`Hop Keep`
announce compression along with the hop list, and moq.pro's lite-07 rollout
has to plan for a lite-07 without hop lists. Only
[Routes and announces](/quest/m1/cluster-routing/routes.md) gates the cut,
not the later federation/P2P integrations. Decided 2026-10-13: routes now
owns the tier/partition correctness tests, so they run before the cut. Peer
grants and metadata-only subscriptions do not gate it: neither is clustering
stability. Both are additive and, per the freeze policy, target whichever lite
wip is current when they land. Decided 2026-10-25: no routing identity proof
gates the cut; every session's receiver, a direct peer included, stamps its
identity instead, and the key and its proof belong to Peer grants. Decided
the same day (maintainer): no cross-path recognition, so the CDN and direct
copies of an origin are separate identities and nothing in lite-07 carries a
stamp between sessions. Simpler: one identity regime.

Decided 2026-10-08: Restart and AUTH join Required, since both change the
lite-07 wire. Compressed tracks joined on 2026-10-17 and landed
([#5928](https://github.com/moq-dev/moq/pull/5928),
[#6228](https://github.com/moq-dev/moq/pull/6228),
[#6230](https://github.com/moq-dev/moq/pull/6230)). SUBSCRIBE_OK carrying live media time was dropped: no reader
was named, and `set_live` covers it.

The cut:

- Every quest under Required has landed.
- The identifier becomes `moq-lite-07` in `rs/moq-net` and `js/net`, in the
  draft (whose text already names the rename), and at every site spelling
  the wip ALPN: `rs/moq-tokio/src/connect.rs` and `listen.rs`,
  `rs/moq-tokio/tests/`, `rs/moq-relay/src/cluster.rs`,
  `rs/moq-relay/tests/integration/smoke.rs`,
  `test/interop/bare-fin.ts`, `test/interop/lite-varint.ts`, and
  `doc/concept/moq-lite.md`; grep for `moq-lite-07-wip` to catch new ones. A wip peer and a final peer refuse each other by ALPN
  rather than misparse; no compatibility shim.
- The draft's lite-07 changelog matches the wire and `just drafts check`
  passes.
- SETUP keeps its stronger rule: #4820 put it under the general Message
  Length cap, which only permits rejection (MAY), while "a receiver MUST
  treat a longer SETUP as a protocol violation" stays. The lite-07 changelog
  names that rejection, not only the cap.

Decided 2026-10-20 (maintainer): [Drop set](/quest/m1/drop-set.md) joins
Required, because it adds `Max Age` to lite-07's SUBSCRIBE_DROP. Only
[Token in band](/quest/m1/auth/token-in-band.md) gates the cut from the auth
line, because it is that line's lite-07 wire work; the rest is API work that
does not block the rename.

The archive line's TRACKS request (a per-broadcast track listing that
replaced SUBSCRIBE_ALL) landed in [#6158](https://github.com/moq-dev/moq/pull/6158)
and [#6167](https://github.com/moq-dev/moq/pull/6167), and the padding stream
type for the m2 browser [padding probes](/quest/m2/probe/padding.md) landed in
[#6233](https://github.com/moq-dev/moq/pull/6233).
The `Time(pts)` range end rides in Subscribe ranges, already Required.

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

## Required

- [Token in band](/quest/m1/auth/token-in-band.md) - lite-07 carries the Auth Stream and UNAUTHORIZED (0x3B)
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - lite-07 carries the route layer: ROUTE per origin node and path-less ANNOUNCE, with the hop list gone
- [Drop set](/quest/m1/drop-set.md) - lite-07's SUBSCRIBE_DROP carries `Max Age`, so a widening re-offers groups reported dropped
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - SUBSCRIBE carries ranges and an order and lite FETCH is gone, in Rust and JS
- [Spike: timestamps replace group sequence numbers](/quest/m1/time-addressing/spike.md) - decides the lite-07 wire without group sequence numbers and adds the implementation quests here
- [Announcement takeover](/quest/m1/announce-takeover.md) - ANNOUNCE_TAKEOVER preserves a proven instance under a new independent route

## Related

- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - the deployment condition that follows this
