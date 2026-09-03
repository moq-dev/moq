# [M] js/net: remaining capability gaps vs rs/moq-net (SETUP role, finish_at and final sequence, range controls, typed errors)

## Goal

Implement and verify the behavior tracked in [#2318](https://github.com/moq-dev/moq/issues/2318)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: SETUP role, frame naming, typed errors,
and start/end cursors landed on dev. Two gaps survive: track end semantics
(finishAt / finished(), still collapsed into close(abort?)), and the
producer-side `announce(prefix)` a Rust publisher has and a browser one does
not.

### Issue context

Where js/net is a strict subset of the Rust model. The subscription-options model is tracked in #2155 and the announce Restart state in #2216; this covers the rest.

- \[ ] **SETUP role parameter**: Rust encodes param 0x3 (Publisher/Subscriber/Both, derived from which origins the client wired up) and the draft documents it, but `js/net/src/lite/setup.ts` only implements probe and path. A JS client always presents as Both and a JS server cannot read a peer's role. `Established.publish/consume` usage can derive it just like Rust's `Role::from_origins`.
- \[ ] **Track end semantics**: Rust distinguishes `finish()` (clean end at live edge), `finish_at(final_sequence)` (declare an end ahead of the live edge), and `abort(err)`, and consumers can await `finished() -> u64`. JS collapses everything into `close(abort?)`: no `finishAt`, and the SUBSCRIBE\_END group number is discarded on the consuming side, so the JS model cannot express or observe lite-05 clean-end semantics. (Interacts with the SUBSCRIBE\_END off-by-one bug, #2309, which should land first.)
- \[ ] **Range/cursor controls**: `start_at`, `end_at`, `get_group` (wait for a live sequence), sync cache peek, and `latest()` have no JS equivalents. Some are relay-only and fine to omit, but `startAt`/`endAt` pair with the missing subscription range fields and a JS player implementing catch-up/DVR hits this wall.
- \[ ] **Typed errors**: Rust has a `#[non_exhaustive]` enum with stable wire codes; JS throws bare `Error` with prose (only `CacheFull` is a class), so consumers string-match to distinguish `NotFound` from `Unauthorized`, and wire reset codes are not surfaced. At minimum add a stable `code` property before locking the API.
- \[ ] **Delete the dead `SubscribeOptions` export** (`js/net/src/track.ts`): introduced by #2167, superseded by `Subscription` in #2170, identical field-for-field, referenced nowhere. Two exported names for one concept invites drift.
- \[ ] **Producer-side `announce(prefix)`**: Rust publishers advertise a prefix route and serve whatever is requested beneath it; JS only consumes prefix routes, so a browser publisher still announces exact paths and cannot advertise a catalog it would materialize on demand.
- \[ ] **Frame field naming**: JS uses `payload` for `Datagram` but `data` for both Frame types, and `@moq/hang`'s container Frame is shaped differently from both `@moq/net`'s and Rust's. Pick `payload` consistently (JS `keyframe`/`duration` are genuine WebCodecs needs and fine as additive fields).

Related: #2155, #2216, #2073.

## Closes

- [#2318](https://github.com/moq-dev/moq/issues/2318) - close this issue when the quest finishes
