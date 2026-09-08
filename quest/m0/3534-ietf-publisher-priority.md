# [M] Publisher priority survives a moq-transport hop

## Goal

A track's publisher priority reaches a moq-transport subscriber the way it
already reaches a moq-lite one. A relay that ingests over either protocol and
serves over moq-transport stamps every group header with the track's priority
instead of 0, a peer that declares DEFAULT_PUBLISHER_PRIORITY (0x21) has it
recorded on the track, and a subgroup header that omits its priority resolves
to that declared default. The audio-ahead-of-video intent of a linear SSAI
publisher is what the next relay sees, and the moqtest conformance requirement
that a relay preserve publisher priority is met.

Boundaries: priority is track-scoped, as `track::Info::priority` and
draft-lcurley-moq-lite define it, fixed for the lifetime of the track. A
subgroup priority that differs from its track's collapses to the track value.
The local send queue keeps ranking by each subscription's own priority.

## Plan

The model has the field: `track::Info::priority` (higher first) rides TRACK_INFO
on moq-lite, and `rs/moq-net/src/ietf/priority.rs` converts to the wire's
lower-first byte. The IETF side ignores it in both directions:

- `rs/moq-net/src/ietf/publisher.rs` builds the group header with
  `publisher_priority: 0`.
- `rs/moq-net/src/ietf/properties.rs` names TIMESCALE (0x08) and
  DEFAULT_PUBLISHER_GROUP_ORDER (0x22); 0x21 falls through the unknown path.
- `rs/moq-net/src/ietf/group.rs` decodes an absent priority flag as a literal
  128.
- `rs/moq-net/src/ietf/subscriber.rs` builds `track::Info::default()` from
  SUBSCRIBE_OK with only timescale and latency.

Steps:

- Properties: add DEFAULT_PUBLISHER_PRIORITY (0x21) beside 0x22. Encode it on
  SUBSCRIBE_OK and PUBLISH from `info.priority` through `priority::to_wire`;
  decode it into `track::Info::priority` on the subscriber through
  `priority::from_wire`. The property block is written from draft-17 on and
  read from draft-16 on, as `Properties::encode` already gates; draft-14 and 15
  have no block at all, so on those drafts the priority travels only in the
  group header and an absent flag resolves straight to the draft's fallback.
- Group header: the publisher stamps `priority::to_wire(track.info().priority)`
  where it reads the timescale today. The decoder resolves an absent flag to the
  track's declared default first and only then to the draft's fallback; confirm
  that fallback against each negotiated draft's text instead of keeping 128 by
  assumption, and cite the section in the type's docs. On the subscriber the
  header value is decoded and then dropped: the model has no per-group
  priority, so an explicit subgroup value that disagrees with the track's
  declared priority never overrides `track::Info::priority`, and a test pins
  that a conflicting header leaves the track's priority unchanged.
- Mirror in `js/net/src/ietf/publisher.ts`, `object.ts`, and `properties.ts`.
- Tests: 0x21 round-trips on SUBSCRIBE_OK on every draft that carries the
  block and is absent from the bytes on the ones that do not; a lite-ingested track
  with priority N serves over moq-transport with header priority `to_wire(N)`;
  a subgroup without the flag decodes to the declared default; a relay
  integration test where hang audio (priority 80) and video (60) arrive at a
  moq-transport subscriber with distinct header priorities. Run the interop
  runner's priority case if it has one.

Additive, on main. No draft change: 0x21 is IETF-registered and moq-lite already
specifies the field.

## Closes

- [#3534](https://github.com/moq-dev/moq/issues/3534) - close this issue when the quest finishes

## Related

- [IETF error codes](/quest/m0/ietf-error-codes.md) - the sibling sweep of the moq-transport registries
