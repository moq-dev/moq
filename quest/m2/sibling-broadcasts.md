# [S] Sibling broadcasts on one clock

## Goal

Several distinct sources (a webcam and a screen share, each of a robot's
cameras, each audio language) publish as sibling broadcasts, one per source,
under a shared path prefix such as `robot/front.hang` and `robot/rear.hang`.
They advertise the same catalog `clock`, so a consumer that discovers them
through one announce prefix aligns them by wall time. The pattern is
documented and tested in Rust and JS. Requested downstream; the robot case
motivates it.

Non-goals:

- A source-grouped catalog, where one broadcast carries several sources each
  with its own renditions. Decided 2026-10-12 by the maintainer: one broadcast
  per camera angle, sharing a clock, instead of another level of catalog
  complexity. Every `video` and `audio` entry stays a rendition of one picture
  or sound.
- A player that renders several sibling broadcasts in lockstep. js/watch
  plays one broadcast; aligning siblings is the consumer's job, through the
  shared clock.
- Sharing a clock across broadcasts from the bindings, which expose no mux
  `Clock`; extend [the shared timebase](/quest/m2/ffi-timebase.md) when a
  binding app needs it.
- Tying a `json` or `binary` track to one source. Those sections are
  application-defined; an application that needs the association defines it,
  presumably in a catalog extension section or by publishing the track in
  that source's broadcast.

## Plan

Most of the pattern ships already, so this is mostly docs and tests:

- Rust: one `moq_mux::Clock` (it is `Copy`) passed to each sibling's
  `catalog::Config::with_clock`. A separate process adopting another
  broadcast's mapping needs the derivative-clock work planned in #5345, which
  is related, not required: one publisher process covers the motivating
  cases.
- JS: js/publish derives every broadcast's clock from
  `performance.timeOrigin`, so siblings on one page already advertise the
  same clock. js/room already publishes `{identity}/camera.hang` and
  `{identity}/screen.hang` and discovers them by announce prefix.

Tests: in Rust, two sibling catalogs on one clock advertise equal `clock`
sections, and frames captured at the same instant map to the same wall time
through `wall_clock`; in JS, two broadcasts on one page advertise the same
clock. Discovery by prefix is already covered by moq-net and js/room.

Docs: edit the existing paragraphs rather than adding a page, likely the
`Clock` entry in `doc/concept/hang.md` and the conferencing use case, to say
that one source is one broadcast and that siblings share a clock.

Decided 2026-10-12: m2, ranked last. Nothing blocks it, and nothing in m1
waits on it.

Public API: none. Wire: none.

## Related

- [Teleoperation](/quest/m2/teleop/README.md) - the robot line whose multi-camera case motivates this
- [Shared timebase in the bindings](/quest/m2/ffi-timebase.md) - shares one offset within a binding broadcast; sharing across broadcasts would extend it
- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - lists renditions from several broadcasts in one catalog, as alternatives of one source rather than as siblings
