# [S] A returning route serves past the cap its old copy was given

## Goal

When a resumed reader's route returns, the reused serving copy asks with the
reader's current resolution, not the capped resolution it was left with when
it was replaced. A range ending at a relative anchor, or at a time that has
already resolved, keeps delivering past the old cap on the returning route,
as it would on a fresh route. Rust first; JS only if the same reuse exists
there.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7364](https://github.com/moq-dev/moq/pull/7364).

- Prompt: "The capped serving copy on a returning route (pre-existing on
  main): fix now, or leave it to the resume redesign?" ✅ Own S quest, m1.
  Not chosen: leave it to [resume as producer replacement](/quest/m2/resume-redesign.md),
  which is blocked until after the cut.
- Fix via the reader's resolution, consistent with [moq-dev/moq#7364](https://github.com/moq-dev/moq/pull/7364)'s widened-backfill
  fix: there a widened backfill asks with the reader's resolution, because
  the copy's own resolution ends at its earlier cap and would pin a range
  ending relative to the latest group.
- The bug: returning to a route reuses its newest copy
  (`rs/moq-net/src/model/resume.rs:1147`), clears `until`, and calls
  `copy.update(&self.mirrored)`. `Copy::wire` (`resume.rs:500`)
  then takes
  the `Sub::Ready(sub) if reader.ranges == self.from` arm and re-reads
  `sub.resolved_ranges()`, which is the capped wire it was last sent. So the
  reused copy stays at its old cap and delivers nothing past it. Re-resolve
  the reused copy against the reader's window instead
  (`sub.resolve_ranges(&reader.ranges)`), as a new backfill does.
- Test: a range ending at a relative anchor (and one at a resolved time end)
  flaps away and back; the returning route delivers groups past the old cap.
  Add the shrunk case to the resume model's regressions if the nightly sweep
  finds one.
- [moq-dev/moq#7364](https://github.com/moq-dev/moq/pull/7364) (merged) edited the same code first.

Public API: none. Wire: none.

## Related

- [Resume as producer replacement](/quest/m2/resume-redesign.md) - would remove copy reuse altogether
