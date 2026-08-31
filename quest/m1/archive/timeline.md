# [M] Archive timeline

## Goal

The existing broadcast timeline becomes a reusable segment engine whose
Window-backed records can be committed only after an asynchronous archive write.

## Plan

Start from `dev`'s implemented model rather than creating another index. It
already produces aligned `{ segment, pts, duration, tracks }` records, supports
arbitrary pacing and non-pacing tracks, accepts explicit `cut(pts)` boundaries,
and represents discontinuous group ranges and keyframe state.

Separate that segmentation state from its current producer-side MoQ sink. It
must accept complete group facts from either the existing producer recorders or
the consumer-side archive writer, then yield a closed segment to an asynchronous
commit sink. The archive sink stores the segment objects, removes unavailable
ranges, and acknowledges the final record; only that acknowledged record enters
the visible timeline.

Replace the never-rolled `moq_json::stream` track with the merged
`moq_json::Window`. One public timeline model covers bounded DVR and unbounded
archives: unbounded use only pushes, while DVR also pops. Group rolls are an
encoding detail and do not surface to consumers.

Preserve the hard HLS property: after the catalog selects a rendition, timeline
records alone are sufficient to render its media playlist without downloading
any media object.

Land the Rust and draft changes against `dev`, building on the `Window`
primitive merged in [moq#3168](https://github.com/moq-dev/moq/pull/3168).
