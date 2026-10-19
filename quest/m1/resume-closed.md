# [S] A resumed track closes without both channels drained

## Goal

A resumed JS track subscriber (`Track.Subscriber` reading a front's route
copies) settles `closed` once its logical track ends cleanly, whether the
application reads groups, datagrams, or both. A group-only reader that reaches
`recvGroup()` EOF on a finite track sees `closed` settle and releases its
`Reader`, copies, and route sources, while a reader of the other channel still
drains what is buffered.

## Plan

Found by a hosted Codex review of #5836 (`resume.ts` `Reader.#finish`). Today a
clean end adds the channel that ran out to `#drained`, and the shell closes only
once both `"groups"` and `"datagrams"` are there. Nothing adds `"datagrams"` for
a reader that never calls `recvDatagram()`, so `closed` stays pending and the
route keeps the reader. Errors already close at once.

- Settle `closed` when the serving copy's track ends cleanly, from the copy's
  own end (its `closed` and declared end), instead of waiting for both read
  channels to be polled to EOF.
- Keep each channel's buffered backlog readable after that: one channel running
  out must not cut off the other's unread data, as `draining groups to their end
  leaves buffered datagrams readable` checks today.
- Check whether a direct (non-resumed) `Track.Subscriber` and the Rust
  `resume::Subscriber` already settle this way, and match them.
- Regression test: a group-only reader of a finite resumed track sees `closed`
  settle and the route release its copy.

Public API: behavior only (`closed` settles for a group-only resumed reader).
Wire: none.

## Related

- [Rust resumed reads](/quest/m1/resume-ordered.md) - the other resume follow-up from #5836
