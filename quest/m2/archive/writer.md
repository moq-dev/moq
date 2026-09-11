# [L] Recording writer

## Goal

A generic `moq-archive` writer consumes explicitly selected tracks from a
`broadcast::Consumer`, feeds their complete groups to the existing segmenter,
stores one object per track per segment, and commits the timeline record only
after the objects it names are durable.

## Plan

The application supplies the broadcast, object prefix, and arbitrary track
registrations, each pacing or non-pacing; the writer does not parse a catalog
or know which tracks are media.

Enrollment creates the track's `.info` before accepting any group. If that
create fails, enrollment fails and no range for the track can enter the
timeline. On `AlreadyExists`, validate the immutable `.info` and accept only matching
parsed `version`, `priority`, and `timescale` values; a priority or timescale mismatch fails enrollment.

Feed the segmenter already on dev (`rs/moq-mux/src/timeline.rs`): take
`Producer::deferred` (:917), enroll each registration through
`Deferred::track` or `pacing_track` (:625-631), and report every complete
group read from the consumer through its `Recorder` (:1096). The application
calls `cut(pts)` (:635) when it knows an aligned keyframe boundary. A segment
may hold many groups per track, especially one-group-per-frame audio. A pacing
track that stops without closing blocks segment completion on purpose; the
application applies its own deadline and calls `cut(pts)` or removes the
track. Storage does not invent a timeout.

Accept new group IDs in strictly increasing order per track; refuse duplicate
or decreasing arrivals. Already accepted groups may finish in any order, so
buffer completion independently and encode in sequence order. Require
nonoverlapping object ranges across segments, including forced cuts; never
backfill a closed segment or move its stalled groups into an overlapping range.

For each `Pending` record from `Deferred::next` (:658), encode and PUT one
object per participating track at `groups/<largest>.<smallest>`, buffering
groups independently of relay retention. The bounds come from the exact
recorded ranges; tracks with no complete groups have no object. After all PUTs settle, drop the ranges of every track whose PUT
failed and commit with `Producer::push` (:948). That needs a per-track
omission on `Pending` beside `Pending::gap()` (:712-719), which clears every
track; add it here. Never publish a range first and hope the relay still has
it. A later segment continues normally after any omission.

Catalog update applicability and cross-track configuration dependencies belong
to [Catalog track identity](/quest/m2/catalog-tracks.md). Recording a catalog
as an ordinary track does not bind its updates to media groups; this writer
does not introduce catalog-specific commit or retention prerequisites.

The archive timeline uses the same object envelope but is not enrolled in its
own records. After pushing segment N and applying retention pops, close the
recording-owned Window group and store the complete groups under the timeline
track's `segments/N` key (19-digit padded N). Commit IDs consecutively,
including all-gap segments. Make it durable before committing N+1; a timeline PUT
failure stops the recording at its preceding durable timeline object. Never
cut a source group. On a clean source end, drain with
`Deferred::finish` (:650), commit the final partial segment, and call
`Producer::finish` (:1015). Do not write a completion marker.

Retention is writer policy. During the next segment commit, use
`Producer::pop` (:983) for expired records before closing that segment's
timeline group. Make it durable, wait the configured grace period, then delete
the corresponding segment objects. No trimming occurs after the final segment
is committed, and timeline objects are never rewritten.

Before a DVR writer resumes input, recover its full retained timeline and
reconcile a complete media-key listing under exclusive prefix ownership.
Wait the deletion grace period from successful recovery, then delete unreferenced `groups/` objects
left by interrupted expiration or uploads. Failed or incomplete recovery must
prevent deletion. Preserve `.info` and timeline checkpoint objects.

Keep archive policy out of protocol libraries. As the native application that
owns its storage and track choices, `moq-cli` attaches the writer to every
import path and enrolls the resulting `broadcast::Consumer` tracks. Downstream
(moq.pro) gateways attach the same writer once it ships in a release.

## Required

- [Archive catalog](/quest/m2/archive/catalog.md)
- [Archive store](/quest/m2/archive/store.md)

## Closes

- [#2281](https://github.com/moq-dev/moq/issues/2281) - close this issue when the quest finishes
