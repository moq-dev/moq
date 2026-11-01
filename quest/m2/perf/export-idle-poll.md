# [M] An export poll nothing woke is cheap

## Goal

A container export poll that no new data woke costs a small constant per
track, instead of walking every track's read path again to return `Pending`.
The `container-export-idle` group in `rs/moq-mux/benches/export.rs` shows the
cost, before and after.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile of the moq-mux TS export tests. It helps those tests and production
exports alike.

[moq-dev/moq#7263](https://github.com/moq-dev/moq/pull/7263) gates repeated
plain `Cursor` reads in moq-net (`track::idle`). Where the idle poll stands on
top of it, in user-space instructions per poll of a live audio export (pinned
core, `profiling` build), measured in
[moq-dev/moq#7504](https://github.com/moq-dev/moq/pull/7504):

- About 5.7k to 10k per track, nearly linear in tracks (TS, 16 tracks: 110k).
- Catalog polling is now about 3%.
- About 45% is `track::Subscriber::poll_group` through `resume::Reader`. A
  track served through an origin front is read as `Inner::Resume`, which the
  `track::idle` gate skips.
- About a third is `group::Consumer::poll_read_frame` on the open group. Its
  `poll_expiry` reaches `Recover::poll_budget`, which syncs the whole resume
  reader on every pending read.
- The rest, about a fifth, is moq-mux bookkeeping per track.

Open: where the early exit goes.

- In moq-net (recommended): extend the `track::idle` gate to `Inner::Resume`
  reads and to group frame reads. Every reader through a front gains. Each
  input to the resume reader and `Recover` must notify, or disarm the gate.
- In moq-mux: one gate per export track around `container::Consumer`. Every
  read below goes through it, so it skips the moq-mux bookkeeping too, but
  nothing outside exports gains. State nothing notifies, such as a `Memory`
  refund, must disarm it.

Public API: none expected.
