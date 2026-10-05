# [S] A fetched group head keeps the live group visible on lite-07

## Goal

On moq-lite-07, a relay that fetches the head of a group it is receiving
mid-group still delivers that group to new arrival-order subscribers, and
frames the publisher writes into it afterward still reach them. Today a fresh
subscriber with no start receives nothing, even after the publisher writes more
frames into the group. lite-05, lite-06, and moq-transport are unaffected.

A long-lived open group (a catalog: snapshot at frame 0, deltas after) is the
case that hurts: a viewer joining the relay never gets the catalog.

## Plan

Scenario, reproduced on `main` 2704e10e2 for `moq-lite-07-wip` only:

1. Publisher P holds open group G with a snapshot and two deltas. Relay R pulls
   P; mesh peer M pulls R.
2. M subscribes to `catalog.json` with a start of `(G, 1)`, then reads its own
   copy from frame 0, so it fetches G's head through R.
3. A fresh reader on R subscribes with no start: it receives no group within
   5 s of virtual time, and a frame P writes into G after 1 s doesn't help.

Test shape (`rs/moq-net/tests`, the mock harness, paused time; run it for every
version and expect group G starting with `snapshot`):

```rust
let start = track::Position { group: sequence, frame: 1 };
let mut sub = peer_remote.track("catalog.json").unwrap()
	.subscribe(track::Subscription::default().with_start(start)).await.unwrap();
let mut group = sub.recv_group().await.unwrap().unwrap();
// The peer reads its headless copy from frame 0: a fetch of G's head through R.
let _ = tokio::time::timeout(Duration::from_millis(500), group.read_frame()).await;

// A fresh reader on R wants the latest group from its snapshot.
let mut sub = remote.track("catalog.json").unwrap().subscribe(None).await.unwrap();
let group = tokio::time::timeout(Duration::from_secs(5), sub.recv_group()).await;
```

Inferred cause, unverified: lite-07 has frame bounds, so R's upstream
subscription starts G at frame 1. M's fetch reaches
`TrackState::insert_group_request` (`rs/moq-net/src/model/track.rs`), whose
`claim_sequence` drops that headless live slot because it can't answer from
frame 0, then commits the fetched group invisible. G's arrival entry keeps the
old stamp, so `poll_recv_group` skips it, and the live copy R keeps filling
from P is no longer in its cache. Settle whether a fetch may replace a live,
visible slot at all, or must keep it arrival-visible and still writable.

Only bites once lite-07 ships: on lite-06, `TrackServe::widen_frame_bounds`
(`rs/moq-net/src/lite/subscriber.rs`) already asks upstream for the head of the
group, so R never holds a headless G.

## Related

- [#4829](https://github.com/moq-dev/moq/pull/4829) - `release`'s backport of the lite-06 head widening
- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - when this starts to bite
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - lite-07 relays fill misses by range, the same cache seam
- [Late lower groups](/quest/m1/lite-late-lower-group.md) - another lite group a subscriber silently never sees
