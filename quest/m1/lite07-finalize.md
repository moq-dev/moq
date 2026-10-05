# [S] Finalize moq-lite-07

## Goal

moq-lite-07 is a published version: it negotiates as `moq-lite-07`, the
`moq-lite-07-wip` identifier is gone, its draft revision describes the frozen
wire, and the next cut of `main` into `release` ships it in Rust and JS. A
deployment pinned to `release` can then roll lite-07 out without a wip wire
changing under it. Wire work after that targets a new `moq-lite-08-wip`.

## Plan

Decided 2026-10-05 in moq.pro's quest audit: finalizing lite-07 is upstream
work with its own quest. Until now nothing upstream owned it, while moq.pro's
mesh and customer rollouts waited on it and the
[mesh condition](/quest/m3/lite07-mesh.md) here waited on moq.pro. This quest
waits on nothing outside the repository; the rollouts and the mesh condition
follow it.

"Final" means:

- Every wire change listed under Required has landed, plus the cache bug
  that only bites once lite-07 ships. That list is the whole freeze set: a
  wire quest not on it, such as
  [Routes and announces](/quest/m1/cluster-routing/routes.md), targets the
  next wip version unless the maintainer adds it (open below).
- The identifier becomes `moq-lite-07` in `rs/moq-net`, `js/net`, the draft
  (whose text already names the rename), `doc/concept/moq-lite.md`, and the
  CLI and relay docs. A wip peer and a final peer refuse each other by ALPN
  rather than misparse; no compatibility shim.
- The draft's lite-07 changelog matches the wire and `just drafts check`
  passes. Later changes start `moq-lite-08-wip` instead of editing lite-07.
- [Wire compatibility](/quest/m1/wire-compat.md) covers lite-07 once a
  release carries it.

Open, for the maintainer:

- Whether [Routes and announces](/quest/m1/cluster-routing/routes.md)'s new
  ROUTE and ANNOUNCE wire gates lite-07 or moves to lite-08. Recommended:
  lite-08. It is [XL] and still in design, and holding lite-07 for it holds
  announce compression for every mesh waiting on lite-07.

- Whether a large in-flight change, such as subscribe ranges or live media
  time, slips to lite-08 so lite-07 ships sooner. Recommended: keep the set
  as planned, and revisit only if one of them stalls.
- Whether released clients offer lite-07 first by default, or accept it while
  still offering lite-06 first for one release. Recommended: servers accept
  it by default and clients keep lite-06 first for one release, so a
  deployment rolls out on its own schedule.
- Whether Rust's `VarInt` must carry the full 64 bits
  ([VarInt codec](/quest/m1/rs2ts/varint-codec.md)) before final, since Rust
  refuses lite-07 values above 2^62-1 today.

Public API: the lite-07 version constant and ALPN lose `-wip`. Wire: lite-07
is frozen; older versions are unchanged.

## Required

- [Remove `--hop`](/quest/m0/broadcast-epoch/hop-removal.md) - lite-07 loses NO_CAPACITY and stamping
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - lite-07 restores SUBSCRIBE_DROP in place of `Stream Count`
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - SUBSCRIBE carries ranges and an order, and lite FETCH is gone
- [Live media time](/quest/m1/subscribe-live-time.md) - SUBSCRIBE_OK carries the publisher's live media time
- [Untimed lite-07](/quest/m1/lite-untimed.md) - an untimed track crosses the wire untimed
- [One route cost](/quest/m1/route-cost.md) - ANNOUNCE carries one cost
- [Fetched heads stay visible](/quest/m1/lite07-head-fetch-arrival.md) - a lite-07 relay still delivers a group whose head it fetched

## Related

- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - the deployment condition that follows this
- [moq.pro: lite-07 on cluster dials](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/lite07.md) - the mesh rollout that adopts the finalized version, and the customer rollout after it
