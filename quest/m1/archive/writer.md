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
timeline. On `AlreadyExists`, read the immutable `.info` and accept only
byte-equivalent metadata; a priority or timescale mismatch fails enrollment.

Feed the segmenter already on dev (`rs/moq-mux/src/timeline.rs`): take
`Producer::deferred` (:917), enroll each registration through
`Deferred::track` or `pacing_track` (:625-631), and report every complete
group read from the consumer through its `Recorder` (:1096). The application
calls `cut(pts)` (:635) when it knows an aligned keyframe boundary. A segment
may hold many groups per track, especially one-group-per-frame audio. A pacing
track that stops without closing blocks segment completion on purpose; the
application applies its own deadline and calls `cut(pts)` or removes the
track. Storage does not invent a timeout.

For each `Pending` record from `Deferred::next` (:658), encode and PUT one
object per participating track, buffering groups independently of relay
retention. After all PUTs settle, drop the ranges of every track whose PUT
failed and commit with `Producer::push` (:948). That needs a per-track
omission on `Pending` beside `Pending::gap()` (:712-719), which clears every
track; add it here. Never publish a range first and hope the relay still has
it. A later segment continues normally after any omission.

Commit prerequisites are new API: an application declares that one enrolled
track's applicable group must be durable before other tracks' ranges in the
same record are published. Store prerequisites first; if one fails, omit its
dependents or fail the record according to application policy. HANG publishers
use this to make the catalog snapshot durable before advertising media that
needs it; the writer compares timestamps and durability but does not parse the
catalog.

The archive timeline uses the same object envelope but is not enrolled in its
own records. After pushing segment N and applying retention pops, close the
recording-owned Window group and store the complete groups under the timeline
track's segment N key. Make it durable before committing N+1; a timeline PUT
failure stops the recording at its preceding durable timeline object. Never
cut a source group. On a clean source end, drain with
`Deferred::finish` (:650), commit the final partial segment, and call
`Producer::finish` (:1015). Do not write a completion marker.

Retention is writer policy. During the next segment commit, use
`Producer::pop` (:983) for expired records before closing that segment's
timeline group. Make it durable, wait the configured grace period, then delete
the corresponding segment objects. No trimming occurs after the final segment
is committed, and timeline objects are never rewritten.

Keep archive policy out of protocol libraries. As the native application that
owns its storage and track choices, `moq-cli` attaches the writer to every
import path and enrolls the resulting `broadcast::Consumer` tracks. Downstream
(moq.pro) gateways attach the same writer once it ships in a release.

## Required

- [Archive catalog](/quest/m1/archive/catalog.md)
- [Archive store](/quest/m1/archive/store.md)

## Closes

- [#2281](https://github.com/moq-dev/moq/issues/2281) - close this issue when the quest finishes
