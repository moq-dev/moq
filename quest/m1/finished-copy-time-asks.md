# [M] A finished relay copy asks upstream for a time start it cannot place

## Goal

A lite-07 relay copy whose publisher finished the track answers a reader's
time start (`Start::Time`, or a live join) that the copy cannot place, by
asking upstream, as a live copy does. Today the reader waits until it gives up.
Example: upstream holds group 15 spanning `[15,20)`, the copy has evicted it
and holds only group 20, and a reader asks for `Time(18)..=18`. Upstream would
answer with group 15. The finished copy never asks.

## Plan

Found by Codex reviewing
[moq-dev/moq#9495](https://github.com/moq-dev/moq/pull/9495), which made the
finished copy serve readers' group misses. The gap is older than that PR: the
finished copy never read readers' time asks.

- A received copy does no floor lookup (`TrackState::looks_up` in
  `rs/moq-net/src/model/track.rs`), so an unplaced time start lands in the
  demand's `Subscription::asks` (and a join in `joins`), not in a fill.
- A live copy forwards them as probes on its subscription (`probes` and
  `join_probes` in `rs/moq-net/src/lite/subscriber.rs`). `FinishedServe` has no
  subscription and watches only `track::Requests`, so nothing forwards them.
- Recommended: `FinishedServe` watches the demand and asks each unplaced time
  on a subscription of its own, as a `Recovery` asks a run, then lets the copy
  record the start it reports (`time_starts`).
- Reproduce first. A scratch run of `finished_copy_history` with sparse keys
  did not get the copy to evict them, so the gap is confirmed only by reading
  the code. Keep the finished copy alive with a live-only reader, since a copy
  nobody reads is released and the next reader asks upstream afresh.
- JS releases a copy once it finishes (and its recoveries end), so a later
  reader asks upstream afresh. Check whether the recoveries' window leaves
  the same gap there.
