# [M] moq-net: a group consumer is one cursor, so an evicted group is skipped instead of ending the export

## Goal

`moq export ts` never exits because the relay or its own cache evicted a
group it wanted. Falling behind the live edge on a congested link is an
expected condition; the container consumer skips to the next buffered group
every time, and only a genuine decode or protocol error on a track ends the
export. The `group::Consumer` API stops having two answers for one question:
`finished()` resolves when this cursor reaches the clean end, and the
producer's total lives in `frame_count()`.

Boundaries: the export still exits on a non-eviction error from any track,
loudly, rather than dropping one PID out of a distribution mux. The latency
coupling that makes `--latency-max` the eviction deadline stays as it is.

## Plan

Branch from dev. `poll_finished` is a published moq-net behavior and this
changes what it means.

What the tree does today, identically on main and dev
(`rs/moq-net/src/model/group.rs`):

- `GroupState::poll_terminal(index)` answers a read: for `fin = Some(total)`
  and `abort = Some(err)` a reader with `index < total` gets `Err(err)`.
- `GroupState::poll_finished()` answers `finished()`: `fin = Some(total)`
  yields `Ok(total)` regardless of the cursor and regardless of a later
  abort. The tests `finished_survives_a_later_abort` and
  `abort_after_finish_keeps_the_clean_end_for_a_drained_reader` pin both
  halves.
- Track expiry (`evict_expired`, `track.rs`) aborts a finished group with
  `Error::Old` and releases its frames. `rs/moq-mux/src/container/consumer.rs`
  `GroupBuffer::poll_aborted` asks `poll_finished` to decide whether a read
  error means "evicted, skip forward"; for a finished-then-expired group it
  says `Ok`, so `Old` propagates through `ts/export.rs` `fill()`, one track's
  error drops the whole `Export`, every subscription cancels as idle within a
  millisecond, and the process exits with `hang: moq error: old`. That is
  #3491 exactly: a few unfinished groups skip, the first finished one is
  fatal.
- dev's expiry path (`expired`, `ended`, `expired_truncates`) already reports
  the cursor's index from `poll_finished`, and `frame_count()` already serves
  the producer total to the IETF publisher's live edge and to fmp4 sequence
  numbers. js/net is already one cursor: `group.ts` `closed` resolves the
  clean end only once every buffered frame is drained.

The work:

- `Consumer::poll_finished` becomes cursor-based: `poll_terminal(self.index())`
  then `Ok(self.index())`, and `GroupState::poll_finished` goes away. A cursor
  that was truncated by an abort gets the abort; a cursor that drained a
  finished group gets the clean end even after a later abort released the
  cache. `frame_count()` is the count accessor; document both in one line
  each.
- `resume::Group::poll_finished` follows the same rule; its cross-seam total
  stays on `resume::Group::frame_count()`.
- Callers: moq-transcode's tests read one frame then assert `finished() ==
  5`; move them to `frame_count()` or drain first. `GroupBuffer`'s comment
  about `poll_finished` surfacing a terminal transport error is reworded.
  Tests that flip: `finished_survives_a_later_abort`,
  `start_at_starts_the_group_later`, and the resume seam-coverage asserts on
  `reading.finished()`.
- The container consumer keeps trusting `poll_aborted`, which now answers
  truthfully. Add the missing regression in `consumer.rs`: a group with two
  frames, `finish()`, one frame read, then `abort(Error::Old)`; the consumer
  skips to the next buffered group. Add the `Lagged` case (front evicted on
  a live group) and assert it skips too; if the cursor rule does not cover it,
  classify that error explicitly rather than propagating it. Add a companion
  asserting a decode error still propagates.
- Verify end to end with the issue's rig shape: two `moq export ts`
  subscribers behind a shared bottleneck run for the full cell with
  evictions logged and no exit.

## Closes

- [#3491](https://github.com/moq-dev/moq/issues/3491) - close this issue when the quest finishes

## Related

- [Group overflow](/quest/m1/group-overflow-abort.md) - retires the head-shedding that produces `Lagged` on an open group
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - the other reason a finished group is aborted after the fact
