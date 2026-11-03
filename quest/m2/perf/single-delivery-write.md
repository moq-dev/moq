# [M] A frame with one delivery writes without a lock

## Goal

Writing a frame that has a single delivery takes no lock and no overlap
check, so 256-frame lite FETCH costs no more instructions than before
[moq-dev/moq#6922](https://github.com/moq-dev/moq/pull/6922), while a second
copy attaching mid-write stays correct.

## Plan

Found 2026-10-28 by [moq-dev/moq#7141](https://github.com/moq-dev/moq/pull/7141),
which brought allocations back to base and recovered about half of #6922's
cost. About +3.6% instructions remains on 256-frame lite FETCH, from #6922's
per-frame locking and overlap checks when writing.

Decided 2026-10-28 (maintainer): accept the residual in #7141 and explore a
lock-free path here, in m2.

- The risk is a second copy attaching to a frame while it is still being
  written. Rule that race out before relying on it.
- Keep the shared reservation and canonical-content rules #6922 added.
- A measured no-win abandons the quest.

Also in scope (decided 2026-10-29, from [moq-dev/moq#7139](https://github.com/moq-dev/moq/pull/7139)):
after #7139, batched `write_frames` still costs about 20% more instructions per
frame than before #6922 (459 against 380; 6 to 12% wall clock), from the same
canonical-storage design. Measure the `group_write_frames/batch32/*` cases
beside lite FETCH, and fix both paths together where they share the cost.

Public API: none. Wire: none.
