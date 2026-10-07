# [S] Finalize moq-lite-07

## Goal

When the maintainer says to cut it, moq-lite-07 becomes a published version:
it negotiates as `moq-lite-07`, the `moq-lite-07-wip` identifier is gone, its
draft revision describes the wire as it stands, and the next cut of `main`
into `release` ships it in Rust and JS. A deployment pinned to `release` can
then roll lite-07 out without a wip wire changing under it.

## Plan

Decided 2026-10-05 in moq.pro's quest audit: finalizing lite-07 is upstream
work with its own quest. Until now nothing upstream owned it, while moq.pro's
mesh and customer rollouts waited on it and the
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
(moq.pro#2210) has to plan for a lite-07 without hop lists.

The cut:

- Every quest under Required has landed, including the cache bug that only
  bites once lite-07 ships.
- The identifier becomes `moq-lite-07` in `rs/moq-net` and `js/net`, in the
  draft (whose text already names the rename), and at every site spelling
  the wip ALPN: `rs/moq-tokio/src/connect.rs` and `listen.rs`,
  `rs/moq-tokio/tests/`, `rs/moq-relay/src/cluster.rs`,
  `rs/moq-relay/tests/smoke.rs`,
  `test/interop/bare-fin.ts`, `test/interop/lite-varint.ts`, and
  `doc/concept/moq-lite.md`; grep for `moq-lite-07-wip` to catch new ones. A wip peer and a final peer refuse each other by ALPN
  rather than misparse; no compatibility shim.
- The draft's lite-07 changelog matches the wire and `just drafts check`
  passes.
- Preserve SETUP's rule: "a receiver MUST treat a longer SETUP as a protocol
  violation and MAY reject it based on the length prefix alone" (65,536
  bytes). The general cap proposed by request caps (#4820) only permits
  rejection (MAY), so it does not replace SETUP's stronger requirement.
  Remove the SETUP sentence only if the general rule requires the same
  rejection. If SETUP keeps its own rule, the lite-07 changelog still names
  that rejection, not only the cap.

Rust's lite-07 varints already carry the full 64 bits the draft specifies,
so no codec work waits on the cut.

Open, for the maintainer:

- Whether released clients offer lite-07 first by default, or accept it while
  still offering lite-06 first for one release. Recommended: servers accept
  it by default and clients keep lite-06 first for one release, so a
  deployment rolls out on its own schedule.

Public API: the lite-07 version constant and ALPN lose `-wip`. Wire: lite-07
is published; older versions are unchanged.

## Required

- [Cluster routing](/quest/m1/cluster-routing/README.md) - lite-07 carries the route layer: ROUTE per origin node and path-less ANNOUNCE, with the hop list gone
- [Remove `--hop`](/quest/m0/broadcast-epoch/hop-removal.md) - lite-07 loses NO_CAPACITY and stamping
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - lite-07 restores SUBSCRIBE_DROP in place of `Stream Count`
- [lite-07 Latest flag](/quest/m1/lite-latest.md) - SUBSCRIBE carries `Latest` apart from its floor
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - SUBSCRIBE carries ranges and an order and lite FETCH is gone, in Rust and JS
- [Live media time](/quest/m1/subscribe-live-time.md) - SUBSCRIBE_OK carries the publisher's live media time
- [Untimed lite-07](/quest/m1/lite-untimed.md) - an untimed track crosses the wire untimed
- [One route cost](/quest/m1/route-cost.md) - ANNOUNCE carries one cost

## Related

- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - the deployment condition that follows this
- [Wire compatibility](/quest/m1/wire-compat.md) - the nightly covers lite-07 once a release carries it
- [moq.pro: lite-07 on cluster dials](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/lite07.md) - the mesh rollout that adopts the finalized version, and the customer rollout after it
