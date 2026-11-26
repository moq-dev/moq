# [L] js/watch: play sibling broadcasts on one wall-clock timeline

## Goal

Several `<moq-watch>` elements that share one `Sync` show the same capture
instant, aligned through each broadcast's catalog `sync.clock`, so a camera
grid or multi-angle event plays in step. A `<moq-watch>` with its own `Sync`
plays exactly as today. Non-goals: correcting a publisher's wrong `sync.clock`
(a group aligns what publishers advertise), and a Rust player counterpart.

## Plan

Reported in [moq-dev/moq#9612](https://github.com/moq-dev/moq/issues/9612) by
j4y-v, who measured 40-300 ms between tiles of three NTP-synced cameras on one
page, each `<moq-watch>` anchoring its own `Sync` on arrival.

Decided 2026-11-25 (maintainer, `/quest-import`):

- ✅ Groups only (rejected: also a standalone wall-anchored mode per element,
  the reporter's part 1; dropping the issue). A shared `Sync` with one member
  gives a single broadcast the same stable capture-to-display latency, without
  a second per-element mode.
- ✅ A group is one shared `Sync` (rejected: a separate `SyncGroup` type that
  gathers members' delays). `Sync` is already a shared clock whose `Track`
  handles join and leave, and whose reference and `"auto"` delay span every
  registered track, so tracks from unrelated broadcasts join the same way.
  - A `Player` takes an optional external `sync`. With none it builds its own
    as today.
  - On a shared `Sync`, each track maps its timestamps through its
    broadcast's `sync.clock` (`@moq/hang`'s wall mapping, converted into the
    `Instant` domain), so every member lands on one wall timeline. A private
    `Sync` keeps today's arrival-based timeline untouched.
- ✅ `<moq-sync>` wraps its `<moq-watch>` descendants and hands them one
  `Sync`, plus the JS form of passing a `Sync` to each player (rejected:
  JS-only; `<moq-watch-group>`, since MoQ already uses "group" for something
  else; a `group="name"` attribute through a document-wide registry, which is
  hidden page-wide state that collides across components; a wrapper with
  `display: contents` leaves CSS grids alone).
- ✅ A member's wall target is (its arrival reference − its wall reference) +
  its existing jitter target, and the shared delay covers the largest of them
  (rejected: re-anchoring the jitter estimator on the wall reference, which
  would change `doc/concept/audio-jitter.md`'s normative observation point,
  moq-audio, and the conformance corpus together). The estimator stays as is.
- ✅ A member whose target exceeds the shared `Sync`'s cap leaves it for a
  private `Sync` and plays on its own, flagged out of sync with a signal and an
  attribute the page can style. It rejoins once back under the cap (rejected:
  the shared delay following the worst member, which slows the whole wall to
  its weakest link). A broadcast without `sync.clock` can't be placed on the
  wall timeline and plays independently, flagged the same way. This also covers
  a source whose clock is seconds off, since its target never converges under
  the cap.
- ✅ The shared `Sync` owns `delay` and the cap, set on `<moq-sync>`, and a
  member's own numeric values don't apply while it is grouped. A member set to
  `delay="instant"` or a nonzero `lookahead` is refused on join with an error,
  since a shared timeline can't honor either (rejected: silently letting the
  group win; refusing any member delay setting, which breaks shared tile
  templates).

Recommendations from the issue, kept unless the code says otherwise:

- Each member joins as soon as it is ready instead of the group waiting for
  all. A member with no frame yet, or with only audio or text, neither blocks
  the group nor raises its delay.
- A tile suspended by `visible` or a hidden tab doesn't hold the delay up, and
  rejoins at the shared playhead.
- A changed `sync.clock` (publisher restart, rebased clock) re-anchors that
  member without dragging the group. A viewer clock step between `Date` and
  `performance` is detected and re-anchors the shared timeline.
- Audio needs no group logic: the page unmutes one member, which follows the
  shared playhead as audio follows `Sync` today.

Tests: two players on one `Sync` with a manual clock, their frames arriving
with different latencies, present the same wall time at the same instant; a
member past the cap leaves and is flagged; one without `sync.clock` plays
alone. End to end, fMP4 or TS ingests (which anchor their clock at arrival)
or `@moq/publish` give aligned `sync.clock`s without waiting on RTSP's fix.
Update `doc/concept/hang.md`, which promises sibling alignment but says
`@moq/watch` plays one broadcast at a time, and `doc/lib/js/watch` for
`<moq-sync>`.

Public API: additive in `@moq/watch` (`Player`'s `sync` option,
`<moq-sync>`, the out-of-sync signal and attribute). No wire change.

## Closes

- [#9612](https://github.com/moq-dev/moq/issues/9612) - watch: play sibling broadcasts in sync through `sync.clock`

## Related

- [Data tracks in sync](/quest/m5/watch-data-sync.md) - also registers new kinds of track with `Sync`
