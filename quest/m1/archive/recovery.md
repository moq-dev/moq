# [M] Resume a recording

## Goal

A writer restarting on a prefix it exclusively owns recovers the retained
timeline and continues the same recording instead of refusing the prefix.

## Plan

`moq_archive::Writer::new` refuses any prefix that already holds a timeline.
Replace that refusal with recovery: list and replay the timeline's
`segments/` objects from a retained checkpoint, then resume the timeline
producer at the next segment ID with the recovered window. That needs a way
to seed `moq_mux::timeline::Producer` with a window and next segment, which it
lacks today.

For a DVR, reconcile a complete `groups/` listing of every recorded track
against the recovered records before accepting input. Wait the deletion grace
period from successful recovery, then delete unreferenced group objects left
by interrupted expiration or uploads. Failed or incomplete recovery or listing
deletes nothing. Preserve `.info` and timeline checkpoint objects.

An unbounded archive resumes the same way without deleting anything. Source
group sequences that restart below the recovered ranges must not produce
overlapping objects; decide whether that refuses the track or starts a new
recording.
