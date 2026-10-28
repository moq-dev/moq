# [S] Ordered reads wait only on owed groups

## Goal

`track::Ordered` waits within `max_delay` for a missing group only while that
sequence is still owed, and moves past it as soon as the track knows it was
dropped, evicted, or skipped. A reader that got every group behaves as today.
No new public API.

## Plan

Today a gap costs an ordered reader nothing: it seeks to the lowest cached
group in range, so a group reordered behind its successor is skipped even
when it would have arrived in budget.

The shelved [#5475](https://github.com/moq-dev/moq/pull/5475) (branch
`codex/ordered-gap-delay`, kept for reference) waited on every missing
sequence until the budget elapsed. The model cannot tell "not yet" from
"never", so it hung or stalled readers; see its
[iteration comment](https://github.com/moq-dev/moq/pull/5475#issuecomment-6173264946).

✅ Decided 2026-10-14 (maintainer):

- Wait only on sequences the track still owes, read from the dispositions
  that SUBSCRIBE_DROP and the track's arrival record
  (`TrackState::arrivals`) keep. Not chosen: an opt-in
  waiting reader (new public API no consumer needs, and opted-in readers hit
  the same holes); fixing #5475 forward (discontinuity markers still stall,
  and unlimited-budget readers still hang on a never-produced group).
- m2. The media path (`moq-mux`'s container consumer) already waits for
  reordered groups within `max_delay`. The archive writer keeps its own
  reader (`rs/moq-archive/src/producer/ordered.rs`, from
  [moq-dev/moq#6160](https://github.com/moq-dev/moq/pull/6160)) that waits
  within `max_delay` for a late group or datagram, since `Ordered` skips gaps.

Open: what "owed" means on a locally produced track, which gets no drops or
dispositions. The motivating case was the archive's local timeline, read in
order with an unlimited budget; [moq-dev/moq#6262](https://github.com/moq-dev/moq/pull/6262)
removed that timeline, so no current reader motivates it.

- Only a subscription owes sequences, so local tracks keep today's ordered
  reads (recommended: no local reader needs to wait, and an unlimited-budget
  local reader can never hang on a hole).
- A local producer's unreleased sequences also count as owed, until it
  writes, skips, or aborts them. Every local skip must then be marked, or an
  unlimited-budget local reader hangs.

Watch for:

- Untimed tracks: the budget can never expire a group, so a hole nothing
  accounts for (a lost datagram, say) must not block forever.
- Deliberate skips: `Producer::discontinuity` and TS passthrough's flagged
  jumps skip a sequence on purpose. A reader passes them at once, not after
  `max_delay`.
- Eviction, expiry, and an aborted group whose slot was reclaimed leave no
  record today; the wait needs the disposition to survive them.
- Readers with unlimited budgets: an evicted or never-produced group must not
  hang them. On #5475 this failed in `moq-hls` history mode, which
  [moq-dev/moq#6517](https://github.com/moq-dev/moq/pull/6517) removed.

Reuse from #5475: its reorder regressions (exact deadline and wakeup, explicit
starts, capped reads, a changed budget), and its snapshot-reader change:
snapshot readers pick the latest group explicitly, since each group replaces
the value, instead of relying on `Ordered` skipping gaps. Add the iteration
comment's two cases: an untimed track with groups 0 and 2..10 keeps
delivering, and an aborted and reclaimed group 1 does not stall group 2 for
`max_delay`.

Update `Ordered`'s doc, which says a gap costs nothing.

Public API: behavior only. Wire: none.

## Related

- [Read backfill](/quest/m1/read-backfill.md) - container reads drop groups below the cursor
