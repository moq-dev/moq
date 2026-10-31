# [M] moqsink reports track demand only once the track is in the catalog

## Goal

A `moqsink` request pad's `track-demand` stays null until its track is listed
in the published catalog, and only then reports `wanted`. An application that
gates its encoder on `track-demand` can no longer stop on the initial
`wanted=false` before the first catalog and hide its own rendition, so the
startup trap documented today is gone. The same holds for a `<track>/<n>`
replacement.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7252](https://github.com/moq-dev/moq/pull/7252) (merged as
[38566f5a84](https://github.com/moq-dev/moq/commit/38566f5a84)).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- [moq-dev/moq#7252](https://github.com/moq-dev/moq/pull/7252) reports demand once the pad negotiates
  (`rs/moq-gst/src/sink/request_pad.rs:474` returns the `Bandwidth`
  snapshot whenever one exists). But the first catalog needs a timed buffer
  and any in-band codec config, so an encoder gated on the initial
  `wanted=false` keeps the rendition out of the catalog and demand never
  arrives. `doc/bin/gstreamer.md:66` warns about it instead.
- Report null until the catalog that lists the track is published, then
  notify `track-demand`. Clear it again on the existing terminal events
  (individual EOS, pad failure, session failure), as today.
- Rewrite the doc paragraph to the new contract and drop the warning. Update
  the element tests (`rs/moq-gst/src/sink/imp.rs` around `:1681`,
  `request_pad.rs` around `:616`) to assert null before the catalog and a
  value after.

Public API: breaking for GStreamer users: `track-demand` stays null longer
(until the track is in the catalog). Wire: none.
