# [S] A widened budget re-offers a stale group a fill answered

## Goal

A Rust arrival reader that writes off a fill-answered group as stale, then widens its budget while the group is still cached, hands that group out, as it does a stale live-feed group. JS matches, with a twin test in both languages.

## Plan

Found by review of [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529). Main already has this bug for a stale filled group `poll_recv_with` picks. That PR's `write_off_oldest` also writes off stale filled groups below the first fresh one, so readers reach it more often.

- The write-off takes the group out of `Misses::filled` (`advance_arrival(Taken::Filled)`) and records it in `record.skipped`. A widening's `reoffer` puts it back in `filter.owed`, but `first_owed` reads only live-feed slots (`Slot::is_read` needs `visible`), so it drops the run and the group is never handed out.
- Recommended fix: remember which skipped keys came from a fill, and have `reowe` hand those back to the misses as filled (or reopen them there), rather than to the owed set.
- Repro: extend `a_stale_filled_group_is_written_off_before_the_newest_goes_out` in `rs/moq-net/src/model/track.rs` to widen the budget after reading group 5, then expect group 1. Check whether JS's key-sorted buffer, which `#reoffer` refills from the cache, already passes.

Public API: none. Wire: none.
