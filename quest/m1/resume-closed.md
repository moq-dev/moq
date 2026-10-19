# [M] A resumed track's end holds across copies and settles every reader

## Goal

A resumed JS track subscriber (`Track.Subscriber` reading a front's route
copies) ends the way a direct one does:

- It settles `closed` once its logical track ends cleanly, whether the
  application reads groups, datagrams, or both. A group-only reader that reaches
  `recvGroup()` EOF on a finite track sees `closed` settle and releases its
  `Reader`, copies, and route sources, while a reader of the other channel still
  drains what is buffered.
- A declared end, once any copy reported it, stays the logical track's end
  across route swaps: `end()` and `finished()` never regress to unknown because
  the new copy hasn't learned it yet, and a copy reporting a different end is
  refused.
- `largest()` resolves `undefined` once the serving copy ends cleanly at its
  declared end, even while the front stays open for other tracks.

## Plan

Found by hosted Codex reviews of #5836.

**Closed with one channel polled** (`resume.ts` `Reader.#finish`). Today a
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

**A declared end across swaps** (`Reader.end()` and `finished()`). Both read the
serving copy, so when copy A has declared the end and copy B takes over before
its subscription learns it, `end()` returns `undefined` and `finished()` waits
on B. Wire publishers can then advertise or wait on the wrong boundary.

- Record the first end any copy declares on the reader and answer from it.
  Check each later copy's end against it, as `resolveTrackInfo` checks track
  properties, and refuse a mismatch.

**Largest after a clean end** (`Reader.largest(seen)`). It returns only once
the route or shell concluded, or the route was refused. A finite serving copy
that reaches its declared end cleanly sets `view.ended` with `closed` `null`,
neither of which ends the wait, so `largest()` waits on a stream that has ended.

- Treat a clean end of the serving copy as terminal there, while a copy that
  failed with its route still waits for the next route.

For all three, check whether a direct (non-resumed) `Track.Subscriber` and the
Rust `resume::Subscriber` already behave this way, and match them.

Regression tests:

- A group-only reader of a finite resumed track sees `closed` settle and the
  route release its copy.
- A swap after copy A declared the end keeps `end()` at that end, and
  `finished()` resolves with it before B learns it.
- `largest()` on a finite resumed track resolves `undefined` once its serving
  copy ends cleanly, with the front still open.

Public API: behavior only. Wire: none.

## Related

- [Rust resumed reads](/quest/m1/resume-ordered.md) - another resume follow-up from #5836
