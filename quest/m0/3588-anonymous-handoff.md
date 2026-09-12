# [S] moq-net: anonymous handoff is proven on the source model

## Goal

An anonymous publisher session that dies without unannouncing is replaced by
the next anonymous session announcing the same path, and a subscriber on a
third anonymous session gets the newcomer's media with no parking and no 404.
On main the relay lingered the dead front, advertised it to the newcomer
before it announced, registered an `ExclusionGuard` for the id it had just
minted, and then read the newcomer's own announce as a reflection: parked
until the linger expired, 404 `dropped` meanwhile, failing interop cells
nightly. dev removed that mechanism; the fix is the model. This quest is the
proof, so the regression cannot come back through a merge. Branch from dev.

## Plan

What the tree does today, all in `rs/moq-net/src/model/origin.rs`:

- `FrontState` (line 1914) holds `next_source`, `sources`, `track_info`,
  `active`, `closed`. No `excluded`, no guard, no linger, no `taints_a_reader`.
- `attach_source` (2078) joins the live front under one lock and the newest
  source becomes active; a front whose sources have all closed is replaced by
  a fresh broadcast rather than spliced into.
- `detach_source` (1990) closes the broadcast synchronously with its last
  source, however the source ended.
- Loop detection is hop-based route filtering: `Consumer::excluding` (3501)
  hides every route whose hop chain contains the peer, enforced in
  `best_route` (3033). The lite publisher applies it at
  `rs/moq-net/src/lite/publisher.rs:119`; the server mints `Hop::random()`
  per accepted session at `rs/moq-net/src/server.rs:217`.
- The relay's `cluster.linger` is a hidden deprecated no-op
  (`rs/moq-relay/src/cluster.rs:485`).

Work: two regression tests in the `origin.rs` test module, beside
`exclude_hides_routes_through_the_peer` (4331) and
`anonymous_routes_never_resume` (4991), using the same rig style.

- Handoff: session A (anonymous hop) attaches a source at a path; a consumer
  from a third anonymous session resolves it. A's source is dropped without
  an unannounce. Session B attaches a source at the same path. The consumer
  resolves again and reads B's media; assert the front closed with A's last
  source and B's front is served immediately, with no stale front at the leaf.
- Reflection, in the lite session harness rather than the origin rig: the
  origin asserts a looping chain never reaches it (`origin.rs:1506`), and the
  drop happens one layer up, where `rs/moq-net/src/lite/subscriber.rs`
  (lines 267 to 305) discards an announce whose chain already names this
  session or its origin. Beside the `SinkSession` tests at `subscriber.rs:1333`,
  a peer that was advertised the path announces it back with the relay's own
  hop in the chain; assert the announce is dropped, the local front keeps
  serving, and the peer's own subscription is still served from it.

Then `just test smoke-full`: the nightly matrix
(`.github/workflows/smoke.yml:11`, `test/smoke/README.md:71`) runs
wire-anonymous clients that hit this path; confirm every cell passes on the
branch. Note the run in the PR.

## Closes

- [#3588](https://github.com/moq-dev/moq/issues/3588) - close this issue when the quest finishes

## Related

- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - anonymous routes rank below identified ones
