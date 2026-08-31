# [L] Recording writer

## Goal

A generic `moq-archive` writer consumes explicitly selected tracks from a
`broadcast::Consumer`, batches their complete groups into aligned segments, and
publishes a timeline record only after the objects it names are stored.

## Plan

The application supplies the broadcast, object prefix, and arbitrary track
registrations. Each registration is pacing or non-pacing, matching the existing
timeline engine; the writer does not parse a catalog or know which tracks are
media.

Enrollment atomically creates the track's `.info` before accepting any groups.
If that create fails, enrollment fails and no range for the track can enter the
timeline. On `AlreadyExists`, read the immutable `.info` and accept only
byte-equivalent metadata; a priority or timescale mismatch fails enrollment.

Refactor the existing `moq-mux::timeline` segmentation state so producer-side
recorders and this consumer-side writer share its cuts, completeness gates,
group-range construction, gaps, timestamps, and final partial segment. The
application calls `cut(pts)` when it knows an aligned keyframe boundary. A
segment may contain many groups per track, especially one-group-per-frame audio.
A pacing track that stops without closing deliberately blocks automatic segment
completion. The application applies its own deadline and calls `cut(pts)` to
force the close or removes that track. The forced segment contains only complete
groups and records the stalled range as a gap. Storage does not invent a timeout.

Buffer complete groups independently of relay retention. When a segment closes,
encode and atomically PUT one object per participating track. After all PUTs
settle, omit failed tracks or missing groups from the record and append it to
the archive's `moq_json::Window`. Never publish a range first and hope the relay
still has it. A later segment continues normally after any omission.

Allow applications to declare that one enrolled track's applicable group is a
commit prerequisite for other tracks. Store prerequisites first; if one fails,
omit its dependent ranges or fail the segment according to application policy.
HANG publishers use this generic mechanism to make the catalog snapshot durable
before advertising media that needs it. The writer compares timestamps and
durability but does not parse the catalog.

The archive timeline is itself stored through the same track machinery. Cut its
active group when the application requests a flush; storage never invents a
maximum age or cuts a source group. On a clean source end, flush the final
partial segment. Do not write a completion marker.

Retention is writer policy. For DVR, pop expired records from the Window, make
the new archive timeline visible, wait the configured grace period, then delete
the corresponding segment objects.

Keep archive policy out of protocol libraries. As the native application that
owns its storage and track choices, `moq-cli` attaches the writer to every
import path and explicitly enrolls the resulting `broadcast::Consumer` tracks.
Downstream (moq.pro) edge gateways attach the same writer once it ships in a
release.

## Required

- [Archive catalog](/quest/m1/archive/catalog.md)
- [Archive timeline](/quest/m1/archive/timeline.md)
- [Archive store](/quest/m1/archive/store.md)

## Closes

- [#2281](https://github.com/moq-dev/moq/issues/2281) - close this issue when the quest finishes
