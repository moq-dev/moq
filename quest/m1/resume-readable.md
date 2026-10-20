# [S] A recovered group is readable only past its replayed frames

## Goal

`group.Consumer.readable()` on a JS group that adopted a replacement copy
resolves only once the next `readFrame()` would return without waiting. Frames
the replacement replays below where the reader already is never count as
readable.

## Plan

Found by a hosted Codex review of #5836. After a stalled group delivered frame
0 and adopted an open replacement that so far buffers only its replay of frame
0, `readable()` in `js/net/src/group.ts` sees a non-empty buffer and resolves.
The replay is dropped only later, in `#readBufferedFrame`'s skip below
`#skip`, so the next `readFrame()` still waits for frame 1.

- Drop the frames below `#skip` in one place that both `readable()` and the
  read path use before they look at the buffer, so readiness and the read agree.
- Regression test, in `group.test.ts` with the recovery hooks: adopt a
  replacement holding only the replayed frame 0, and `readable()` stays
  pending until frame 1 arrives.

Public API: behavior only. Wire: none.

## Related

- [Resumed close](/quest/m1/resume-closed.md) - another resume follow-up from #5836
