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
- **Errors stay fatal.** A replacement no longer ends the old subscriptions,
  so an error on the catalog or a track posts the session error as today.
  The earlier rule treating `Unroutable` as a switch in flight is dropped: it
  could not tell a switch from a dead route and could hang.
- **Cutover.** Cancel the old pumps as soon as the `Restart` is seen, without
  waiting for their subscriptions to end.
- **Pads (decided 2026-10-07, reconfirmed after the Restart re-scope).** Keep each pad by rendition name across a
  switch, so a `gst-launch` pipeline linked by name (`s.video_0 ! queue`)
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
- **Linger (decided 2026-10-07).** A `linger` property, default 0. After
  `End`, keep the pads idle for that long; a `Start` in the window resumes on
  the same pads, otherwise EOS. This covers a lite-06 or moq-transport
  restart whose END and START are not coalesced into a `Restart`. While
  lingering is possible, a track that ends does not push EOS or drop its pad,
  since the old tracks can finish before the `End` arrives; EOS goes out when
  the window expires or the next catalog drops the rendition. The default
  keeps today's ending.
- **Where.** The logic stays in `rs/moq-gst/src/source`. If
  [Apps](/quest/m0/broadcast-epoch/apps.md) lands a shared helper for
  following announcements first, use it instead.
- **Test.** An in-process test republishes the path under a newer epoch while
  the old publisher stays up: the new run's buffers render through a synced
  sink, a pad linked by name keeps flowing, and the bus carries no error.
  Cover an epochless source change that also switches, a same-epoch
  re-announce (`Update`) that does not, a switch with new caps that keeps the
  pad, and an `End` then `Start` that resumes on the same pads with `linger`
  set (the old tracks finish before the `End` is delivered) and ends with it
  at 0. Drive the linger window with mocked time.

Update the `moqsrc` section of `doc/bin/gstreamer.md`: a restart keeps each
pad by rendition name, a format change keeps its pad, a dropped rendition gets
EOS, and `linger`.

Public API: a new `linger` property on `moqsrc`. Wire: none.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - delivers the `Restart` announce event and sticky subscriptions this follows
