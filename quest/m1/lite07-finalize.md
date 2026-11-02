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
gates the cut; the key and its proof belong to Peer grants, and lite-07
routing identities stay declared and unverified. Decided 2026-10-26
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
- The identifier becomes `moq-lite-07` in `rs/moq-net` and `js/net`, in the
  draft (whose text already names the rename), and at every site spelling
  the wip ALPN: grep the repository for `moq-lite-07-wip`. A wip peer and a
  final peer refuse each other by ALPN rather than misparse; no
  compatibility shim.
- The draft's lite-07 changelog matches the wire and `just drafts check`
  passes.
- SETUP keeps its stronger rule: #4820 put it under the general Message
  Length cap, which only permits rejection (MAY), while "a receiver MUST
  treat a longer SETUP as a protocol violation" stays. The lite-07 changelog
  names that rejection, not only the cap.

Decided 2026-10-20 (maintainer): [Drop set](/quest/m1/drop-set.md) joins
Required, because it adds `Max Age` to lite-07's SUBSCRIBE_DROP. Nothing
else from the auth line gates the cut: its lite-07 wire work, the SETUP Token
parameter, has landed, and the rest is API work that does not block the rename.

The archive line's TRACKS request (a per-broadcast track listing that
replaced SUBSCRIBE_ALL) landed in [#6158](https://github.com/moq-dev/moq/pull/6158)
and [#6167](https://github.com/moq-dev/moq/pull/6167), and the padding stream
type for the m2 browser [padding probes](/quest/m2/probe/padding.md) landed in
[#6233](https://github.com/moq-dev/moq/pull/6233).
Decided 2026-10-30 (maintainer, on
[moq-dev/moq#7390](https://github.com/moq-dev/moq/pull/7390)): those probes
pad with datagrams rather than streams, and padding datagrams are supported
only on lite-07. Lite-07 now defines the padding datagram type and both receivers
discard it.
The `Time(pts)` range end rides in the subscribe-ranges children already Required.

Decided 2026-10-27 (maintainer): of [Subscribe ranges](/quest/m1/subscribe-ranges/README.md),
only the children that change the lite-07 wire gate the cut:
[Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md),
[JS ranges](/quest/m1/subscribe-ranges/js.md), and
[Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md). The
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

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - lite-07 carries the route layer: ROUTE per origin node and path-less ANNOUNCE, with the hop list gone
- [Drop set](/quest/m1/drop-set.md) - lite-07's SUBSCRIBE_DROP carries `Max Age`, so a widening re-offers groups reported dropped
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - a relay fills a lite-07 upstream through the subscription
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - JS speaks lite-07 ranges
- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - lite FETCH is gone from lite-07, in Rust and JS
- [Lite-07 by start](/quest/m1/time-addressing/lite07.md) - lite-07 carries group starts, previous-start links, GROUP_END, and time bounds instead of sequence numbers
- [@moq/net by start](/quest/m1/time-addressing/js.md) - JS speaks the same lite-07 wire
- [A job's exact route continues its claim's readers](/quest/m1/announce-takeover.md) - the job's route inherits the claim's identity, with no new wire message

## Related

- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - the deployment condition that follows this
