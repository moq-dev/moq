# [S] A rejoined IETF reader keeps its open group

## Goal

On moq-transport-19 and 22, a client that leaves a track and rejoins while a
group is still open keeps receiving that group's later frames. Today the
rejoined client's open group ends with `Ok(None)` before the next frame.

## Plan

Repro: in `rejoin_mid_group_keeps_the_head_for_later_readers`
(`rs/moq-net/tests/rejoin.rs`, from #4828), write a frame `a1` to the open
group after the later relay reader's head check, and assert the rejoined
reader gets `a1`. It passes on every lite version and fails on IETF 19 and
22. A first subscription on IETF does receive the later frames, so the loss
is specific to the rejoin.

When fixed, keep the later-frame assertion in that test for every version.

Public API: none. Wire: none.

## Related

- [Fetched heads stay visible](/quest/m1/lite07-head-fetch-arrival.md) - another mid-group join that loses part of a group
- [Rejoin after the copy goes idle](/quest/m1/test-flakes-2/rejoin-idle-race.md) - a different IETF rejoin bug, serving a stale cache
