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

Reproduced 2026-11-03 on `main` at `801fd9a2f2f5c88371fa833d805620d97d372465` with event-driven
`CatalogRun` probes in `sink/imp.rs`, run through
`quest gate nix develop --command just rs test -p moq-gst` with a nextest
filter. No readiness sleeps or retries:

- H.264 CAPS reports `moq-track-demand { wanted: FALSE }` while the catalog is
  pending. An encoder that runs only for null demand or `wanted=true` sends
  nothing and never publishes its catalog. Sending one unconditional timed
  keyframe immediately publishes a catalog listing `camera`.
- An `application/octet-stream` pad reports `wanted=false`, then `wanted=true`
  when directly subscribed, while the catalog remains pending. This is
  intentional catalog behavior: `sink/pad.rs` builds `Sink::Opaque` without
  a catalog entry; `Sink::demand` currently includes it.

The probes were temporary diagnostics, not committed failing tests. No source,
user-facing docs, migration notes, or wire changes have been made yet.

## Waiting

- Maintainer: settle demand reporting for opaque `application/octet-stream`
  pads before implementation. They deliberately have no catalog entry, so the
  goal's unqualified request-pad contract would make their demand permanently
  null. Options: (1) gate media and text on catalog publication and preserve
  opaque demand at negotiation (recommended: fixes encoder startup without
  removing useful demand for direct subscribers); (2) apply the contract to
  every pad, leaving opaque demand permanently null; (3) give opaque tracks a
  catalog representation, which expands the quest's scope and may affect the
  wire. Record the choice here and in the PR, then implement and test the
  chosen contract, including replacements and terminal events, update
  GStreamer docs/migration notes, run checks/interop, and run `/quest-review`.
