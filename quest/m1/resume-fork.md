# [S] A forked resumed subscriber follows route swaps

## Goal

A fork of a resumed JS track subscriber (`Track.Subscriber.fork()` on a
subscriber reading a front's route copies) is itself a logical reader. It
follows the same-epoch route swaps its parent does, so a read it has open
continues on the new route instead of ending with the old one.

## Plan

Found by a hosted Codex review of #5836. `ResumeSource.fork` in
`js/net/src/resume.ts` returns `serving.sub.fork(options)`, a subscriber tied
to the copy serving at fork time. The IETF draft-20 FILL path in
`js/net/src/ietf/publisher.ts` forks the track to drain cached and arriving
content. If the old route fails while the fill reads an incomplete group or
waits for an announced one, the parent recovers through the new route and the
fill ends on the old one.

- Make a resumed `fork` open another logical reader on the same `Route`, as
  `hooks.resumeSubscriber` does for the parent, with the fork's own
  subscription, instead of delegating to the serving copy.
- Rust has no subscriber fork today, so nothing to mirror. Check that the IETF
  FILL path in `rs/moq-net` reads through the logical track.
- Regression test: fork a resumed subscriber, swap routes while the fork reads
  an open group, fail the old route, and the fork reads the rest of the group
  from the new one.

Public API: behavior only. Wire: none.

## Related

- [Resumed in-flight groups](/quest/m1/resume-in-flight.md) - another resume follow-up from #5836
