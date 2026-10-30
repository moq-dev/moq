# [M] Resume delivers late groups below its resume point

## Goal

After a route change, a resumed reader still gets a group below the newest
group it handed out when that group arrives late, reordered or in flight at
the failover, as one plain cursor on the logical track would. Today those
groups are written off. The resume model test drops its `resumed`
relaxation in Rust and `@moq/net` and still passes its gate seeds and a
nightly sweep. Nice to have, not required: the written-off policy is
correct as documented, and this is a bonus.

## Plan

Decided 2026-10-29 (maintainer, on
[moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947)): keep the
current resume point, asking a new route from the newest group handed out,
and deliver the late lower groups as a follow-up.

- Backfill only the gaps below the resume point, from the serving route
  through the existing backfill copy, bounded by the reader's own budget.
  Do not replay groups the reader already handed out.
- Retire the tests that encode the write-off, such as
  `a_widened_floor_stays_above_the_replacement_resume_point`, in favor of
  their delivering counterparts.
- If [resume as producer replacement](/quest/m2/resume-redesign.md) lands
  first, readers become plain cursors and this falls out of it: verify
  with the model and delete this quest.

Public API: none. Wire: none.

## Related

- [Resume as producer replacement](/quest/m2/resume-redesign.md) - makes readers plain cursors, which delivers these groups for free
