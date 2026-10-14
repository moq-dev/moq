# [S] Ordered reads wait only on owed groups

## Goal

`track::Ordered` waits within `max_delay` for a missing group only while that
sequence is still owed, and moves past it as soon as the track knows it was
dropped, evicted, or skipped. A reader that got every group behaves as today.
Only a subscription owes sequences, so a locally produced track has nothing
owed and its ordered reads are unchanged. No new public API.

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
  that [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) and
  [Tail arrivals](/quest/m1/tail-arrivals.md) record. Not chosen: an opt-in
  waiting reader (new public API no consumer needs, and opted-in readers hit
  the same holes); fixing #5475 forward (discontinuity markers still stall,
  and unlimited-budget readers still hang on a never-produced group).
- m2, because no reader needs it yet. The media path (`moq-mux`'s container
  consumer) already waits for reordered groups within `max_delay`, and the
  archive writer reads media in arrival order.

Watch for:

- Untimed tracks: the budget can never expire a group, so a hole nothing
  accounts for (a lost datagram, say) must not block forever.
- Deliberate skips: `Producer::discontinuity` and TS passthrough's flagged
  jumps skip a sequence on purpose. A reader passes them at once, not after
  `max_delay`.
- Eviction, expiry, and an aborted group whose slot was reclaimed leave no
  record today; the wait needs the disposition to survive them.
- History reads with unlimited budgets (`moq-hls` `history::read`, the
  archive's local timeline): an evicted or never-produced group must not hang
  them. `history_mode_recovers_evicted_timeline_groups` and
  `history_mode_refuses_a_retained_recording` failed on #5475.
- Locally produced tracks (the archive's timeline, say) receive no drops or
  dispositions, so they never wait. If a consumer later needs a local
  producer to reorder, bring that to the maintainer rather than inferring
  owed sequences.

Reuse from #5475: its reorder regressions (exact deadline and wakeup, explicit
starts, capped reads, a changed budget), and its snapshot-reader change:
snapshot readers pick the latest group explicitly, since each group replaces
the value, instead of relying on `Ordered` skipping gaps. Add the iteration
comment's two cases: an untimed track with groups 0 and 2..10 keeps
delivering, and an aborted and reclaimed group 1 does not stall group 2 for
`max_delay`.

Update `Ordered`'s doc, which says a gap costs nothing.

Public API: behavior only. Wire: none.

## Required

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - every stream group in a subscription arrives or is named dropped, including publisher skips
- [Tail arrivals](/quest/m1/tail-arrivals.md) - per-track live dispositions survive cache eviction

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - late stream headers are not proof that a group was dropped
- [Read backfill](/quest/m1/read-backfill.md) - container reads drop groups below the cursor
