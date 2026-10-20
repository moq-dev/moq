# [M] A local subscriber reads only its frame bounds

## Goal

A `track::Subscriber` whose ranges start or end partway through a group reads only the
frames they cover: a start at group 4 frame 5 hands out group 4 from frame 5, and an
end mid-group stops there, whatever another subscriber widened the aggregate to. Today
the model filters whole groups and hands out every cached frame, so local readers
(moq-ffi, moq-mux, in-process apps) see frames they never asked for.

## Plan

Found by Codex on #5929 and reproduced on `main` (824179a3a) as well as on that PR: a
subscription starting at frame 5 of group 0 is handed the group from frame 0. #5929's
`Window` collapses each range to group sequences, but the leak predates it: `main`'s
cursor never positioned a delivered group either. Only the datagram path checks the
start frame (`mid_group` in `Cursor::poll_recv_datagram`).

The wire is not affected: the lite publisher positions each served group per
subscription (`position_group` in `rs/moq-net/src/lite/publisher.rs`, and the same
shape in `js/net/src/lite/publisher.ts`).

- Keep each range's absolute frame bounds in the `Window` beside its group runs.
- Position every group the cursor hands out (`group::Consumer::start_at`/`end_at`), on
  the arrival, ordered, and re-offer paths, using the bounds of the range that covers it.
  A group whose head is gone and that was asked for from frame 0 stays a miss, as
  `position_group` does today.
- Then let the lite publisher rely on the model's positioning instead of its own, if
  that is simpler.
- Check `js/net`'s `track` subscriber for the same shape and mirror the fix.

Regression test: a start at frame 5 and an end at frame 3 of a cached group, each read
through `recv_group` and `ordered()`.

## Related

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - the per-subscriber ranges this extends to frames
