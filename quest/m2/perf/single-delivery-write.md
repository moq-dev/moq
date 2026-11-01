# [M] Recover the rest of #6922's per-frame write cost

## Goal

256-frame lite FETCH and batched `write_frames` cost no more instructions per
frame than before [moq-dev/moq#6922](https://github.com/moq-dev/moq/pull/6922),
keeping its shared reservation and canonical-content rules, or the measured
residual is accepted and this quest deleted.

## Plan

[moq-dev/moq#7508](https://github.com/moq-dev/moq/pull/7508) made a frame's
creating delivery write, admit, and release without the publication lock or an
overlap check until something attaches, and skipped the per-frame canonicalize
call on a group without shared content. That took 76k instructions (1.55%) off a
256-frame lite FETCH round (1 publisher, 1 viewer): 4.911M on main to 4.835M.

Measured with user-space instruction counts on a pinned P-core, against
`c67c124b20`, the parent of #6922:

- **Lite FETCH.** Base 4.594M per round; #6922 raised it to 4.970M and
  [moq-dev/moq#7141](https://github.com/moq-dev/moq/pull/7141) brought it to
  4.757M. About 87k of #6922's 163k residual remains after #7508. A symbol
  profile spreads it thinly: building and dropping the larger `FrameContent`
  (about 40k), the commit path (`frame::Producer::finish`, `GroupState::commit`,
  about 30k), and reader and copy costs. Locking and overlap checks are gone.
- **Batched writes.** `group_write_frames/batch32/512` is 422 instructions per
  frame on base and 474 on main, unchanged by #7508: the batch path takes no
  frame lock. Padding base's `Entry` to main's size costs 13 per frame there, but
  shrinking main's owner slot to one pointer saves only 2.6, and inlining
  `GroupState::commit` or `canonicalize` into `write_frames` costs 3% more, so the
  rest is code layout around `write_frames` rather than a removable step.

The candidates left are a smaller `FrameContent` for the single-delivery case
(moving the canonical-only fields behind a lazily allocated part, at one more
allocation per canonical frame) and a `write_frames` layout that keeps base's
inlined per-frame body.

Public API: none. Wire: none.
