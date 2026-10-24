# [M] Finite drain for a replaced run's HLS recording

## Goal

When a broadcast's publisher announces `Restart`, a moq-hls consumer that is
still recording the old run finishes that recording at a finite, defined
boundary, even if the old publisher keeps sending media. The successor run
starts at once and never waits on the old drain. The old recording keeps every
segment listed before the boundary and nothing after it.

## Plan

Today an open subscription stays on the old run until its route goes, so an
old publisher that keeps sending can keep its recording open indefinitely.
Catalog EOF and announce `End` already finalize a recording cleanly
([moq#6087](https://github.com/moq-dev/moq/pull/6087)); a replaced run has no
equivalent cutoff.

Decided 2026-10-24 (iteration, on the merits):

- The boundary is the timeline row: the old run's windows end when the export
  observes the `Restart`, as a closed sibling's window ends today. Rows listed
  before it drain from the retained instance; rows the old publisher lists
  after it are refused. The cutoff is finite because the listed set is fixed
  at `Restart`, and it adds no timer or constant.
- Rejected: a timeline-head cutoff at the successor's first media time. It
  compares timestamps across two independent publishers, whose timelines need
  not relate.
- Rejected: a group cutoff. A row can span several groups (audio packs many),
  so cutting at groups can truncate a listed segment.
- Rejected: a grace window in media time. The old publisher controls its
  timestamps, so it could hold the window open; a wall-clock bound would be a
  constant whose only effect is a refusal.
- Rejected: an unbounded drain of the old run; an immediate cutoff at
  `Restart` that discards listed rows; treating a catalog error as media EOF.
- A cutoff recording is complete: its cursor returns `Ok(None)` after the last
  listed row, as after `End`. Keep this consistent with the end semantics the
  [cursor audit](/quest/m1/hls-teardown.md) settles.
- No new public API: the boundary shows up as the cursor's end. No wire change.

Tests: an old publisher that keeps sending after `Restart` still finalizes
with exactly the rows listed before it; with the old run's drain held
blocked, the successor's recording begins as soon as `Restart` is processed;
catalog EOF and catalog-error behavior are unchanged.

## Related

- [HLS cursors error on every incomplete recording](/quest/m1/hls-teardown.md) - decides which window closes are clean finishes
- [HLS recorder in the CLI](/quest/m1/hls-record-cli.md) - the recorder that would see a replaced run
