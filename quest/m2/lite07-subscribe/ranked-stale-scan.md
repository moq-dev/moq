# [M] Stop the ranked read rescanning a stale live backlog per group

## Goal

A ranked read (`Pick::Ranked`, which a lite-07 publisher uses while a group waits for stream credit) costs at most a constant number of staleness checks per group it hands out. Today, with a short-budget newest-first live range over an unlimited history range, a backlog of N groups that aged out of the live range is rescanned against the live budget on every read before one history group goes out, so draining it costs O(N²) checks under the track's shared lock. The cache bounds N, so this costs time but never grows without bound. Rust only: the JS publisher has no ranked read.

## Plan

- `Cursor::best_unread` (`rs/moq-net/src/model/track.rs`) calls `first_unread` once per range on every read, and `first_unread` walks the range's run past every group `wants` rejects. Remember, per range, how far a scan proved the run stale, and resume from there on the next read. A group's staleness can only change when its successor changes (a later group arrives, or the successor aborts and a newer one takes its place), the edge moves, or the range's budget changes, so reset the mark on any of those: late arrivals and aborts below the mark, a range edit or retune, and a drift edge change.
- Recommended over a partition search by age, which needs an ordered index the cache does not keep.
- Test, failing first on a cost counter: a backlog of N groups stale for the live range and owed to history, drained by ranked reads, checks O(N) groups in all. Two more land a late group inside the proven-stale stretch, and abort the successor of a group in it, each expecting the group that turns fresh to be picked.
- Measure with the track benches before and after, swept over backlog size.

Public API: none. Wire: none.

## Related

- [Re-rank in-flight groups when a relay places a provisional live join](/quest/m2/lite07-subscribe/inflight-placement-rank.md) - the other ranked-scheduling follow-up from the same PR
