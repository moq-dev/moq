# [S] A truncated group never reports a clean end

## Goal

`group::Consumer::finished()` on a spliced group succeeds only at the
original group's real end. When the continuation past a bounded copy can never
arrive (pruned, or the new route skip-declared the group), `finished()` fails
with the error that killed that route instead of returning `Ok(cap)`, so a
caller can't mistake a truncated group for a complete one.

## Plan

`poll_finished` in `rs/moq-net/src/model/resume.rs` returns `Ok(cap)` when
`poll_covering` finds no route for the seam, and its doc comment calls that
"the logical group's total frame count". That contradicts `group.rs`, where
`finished()` answers for the cursor and only a real end is clean.
[#4651](https://github.com/moq-dev/moq/pull/4651) fixed the same mismatch on
the path where the continuation answers.

Decisions (2026-10-01):

- ✅ `finished()` means the original group's end; a truncated group fails.
  Rejected: redefining `finished()` as wherever the copy stopped, and dropping
  the mismatch as too small.
- ✅ The error is the one the dead route recorded (`bury()`), falling back to
  an existing variant. Rejected: a new public `Error::Truncated`.

Work: change the no-route arm, fix the doc comment, and rewrite
`finished_resolves_for_a_pruned_bounded_group` and
`finished_resolves_when_the_successor_skips_the_seam` to expect the error.
Check `moq-mux`'s consumer still drops such a group as aborted.

Public API: behavior only (an error where `Ok` was returned). Wire: none.

## Related

- [Resumed groups](/quest/m1/resume-latest.md) - same `poll_finished` subscription path
