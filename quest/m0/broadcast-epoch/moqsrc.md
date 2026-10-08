# [M] moqsrc follows a restart

## Goal

When the source behind `moqsrc`'s path restarts (an announce `Restart`), the
pipeline keeps running and switches to the new broadcast: a fresh catalog on
the same pads, with no error on the bus. Once subscriptions are sticky,
`moqsrc` would otherwise keep playing the replaced broadcast until its route
goes, then end or error. A broadcast that ends with no successor still ends as
it does today, unless the opt-in `linger` holds it.

## Plan

Decided in planning (2026-10-06, after #4955 found that no quest covered
`moqsrc`), re-scoped onto [Restart](/quest/m0/broadcast-epoch/restart.md)
(2026-10-07):

- **Follow announcements.** `moqsrc` follows the path's announcements
  (`origin::Consumer::announced`) rather than resolving once with
  `routed_broadcast`. It plays on `Start`, switches on `Restart` by
  re-requesting the path (which resolves the new route), and ends on `End`.
  An `Update` (a re-price or same-epoch failover) changes nothing, so it
  stays a seamless mid-group resume. Restart owns deciding what is a source
  change, so `moqsrc` never compares epochs itself.
- **Errors keep today's handling.** A replacement no longer ends the old
  subscriptions, so a catalog error posts the session error and a track error
  logs and ends that pad, as today. The earlier rule treating `Unroutable` as
  a switch in flight is dropped: it could not tell a switch from a dead route
  and could hang. Only `linger` below defers an error, and only for its
  window.
- **Cutover.** Cancel the old pumps as soon as the `Restart` is seen, without
  waiting for their subscriptions to end.
- **Pads (decided 2026-10-07, reconfirmed after the Restart re-scope).**
  Keep each pad by rendition name across a switch, so a `gst-launch` pipeline linked by name (`s.video_0 ! queue`)
  keeps playing. Fresh pads were rejected: pad ids come from a process-wide
  counter, so a new run's pads get new names, stay unlinked, and the pipeline
  goes idle silently. Pad state outlives a pump: a pump hands its pad back to
  the session instead of removing it. On the switch, push stream-start, the
  new caps (even when they differ, so a codec change fails loudly as
  not-negotiated rather than going quiet), and a segment whose `base` is the
  current running time, so a synced sink does not drop the new run (PTS
  restarts at 0) as late. An in-run caps or container change keeps its pad
  the same way. A rendition the new catalog drops ends with EOS; a new or
  renamed one gets a new pad.
- **Linger (decided 2026-10-07).** A `linger` property, default 0, that
  covers a lite-06 or moq-transport restart whose END and START are not
  coalesced into a `Restart`, and a crashed or withdrawn source. While
  `linger` > 0, a track that ends or loses its source (the route went,
  `Dropped`, or the upstream session closed) hands its pad back without EOS,
  since tracks, the catalog, and `End` arrive on different streams in either
  order. A rendition the catalog already retired still drains to EOS as today.
  The window starts on the first of `End`, the catalog finishing (a prefix
  can stay announced after its output finishes), or the catalog losing its
  source. A `Start` or `Restart` in it resumes on the same pads. At expiry
  every held pad gets EOS, and the session error posts only if the catalog
  lost its source; a pump still streaming ends as it would without linger. A
  held track whose catalog and path stay live waits the same bound from its
  own end, then ends its pad as it would at 0. The bound keeps this from becoming a
  silent hang. At 0 nothing changes from today.
- **Where.** The logic stays in `rs/moq-gst/src/source`. If
  [Apps](/quest/m0/broadcast-epoch/apps.md) lands a shared helper for
  following announcements first, use it instead.
- **Test.** An in-process test republishes the path under a newer epoch while
  the old publisher stays up: the new run's buffers render through a synced
  sink, a pad linked by name keeps flowing, and the bus carries no error.
  Cover an epochless source change that also switches, a same-epoch
  re-announce (`Update`) that does not, a switch with new caps that keeps the
  pad, and with `linger` set: an `End` then `Start` that resumes on the same
  pads (media FIN, then catalog FIN, then `End`, then `Start`), an old
  publisher's session closed (not finished) over a relay that resumes on the
  next `Start`, and a finished output under a still-announced prefix that
  gets EOS by the deadline, and a lone track losing its source while its
  siblings and the catalog stay live, which ends only that pad. With
  `linger` at 0, each of these ends or errors as today. Drive the linger window with mocked time.

Update the `moqsrc` section of `doc/bin/gstreamer.md`: a restart keeps each
pad by rendition name, a format change keeps its pad, a dropped rendition gets
EOS, and `linger`.

Public API: a new `linger` property on `moqsrc`. Wire: none.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - delivers the `Restart` announce event and sticky subscriptions this follows
