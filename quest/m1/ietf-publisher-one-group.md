# [L] Our moq-transport publishers serve at most one group

## Goal

The Rust (`rs/moq-net/src/ietf/publisher.rs`) and JS
(`js/net/src/ietf/publisher.ts`) moq-transport publishers answer FETCH and
draft-22 fills only for one group:

- a standalone FETCH whose start and end are in the same group,
- a Relative Joining FETCH at offset 0 (draft 18), or
- a fill of the largest group's head (draft 22).

Any wider FETCH is refused with NOT_SUPPORTED before FETCH_OK. A wider fill
gets the draft's fill-failure signal (its fetch stream reset right after
FETCH_HEADER, as `FillServe::Unsupported` already does), so its SUBSCRIBE
stays up. FILL_TIMEOUT is accepted and ignored on both sides.

Non-goals: our subscribers. `rs/moq-net/src/ietf/subscriber.rs` keeps sending
range FETCH and multi-group fills upstream, one per run of misses, as
[moq-dev/moq#6724](https://github.com/moq-dev/moq/pull/6724) does. Sending
is cheap and correct, since the upstream answers with its own gap markers.
Joining FETCH and the largest-group fill are unchanged.

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "what's not worth
supporting in IETF").

Why: answering a range authoritatively means never serving from an
incomplete cache, so the publisher must know every group in the range or
fetch every gap upstream. Since June, 21 of about 234 IETF fixes were FETCH,
join, or fill, mostly in the range logic: end locations
([moq-dev/moq#6295](https://github.com/moq-dev/moq/pull/6295),
[moq-dev/moq#5487](https://github.com/moq-dev/moq/pull/5487)), mid-group
starts ([moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)),
credit ([moq-dev/moq#6702](https://github.com/moq-dev/moq/pull/6702)),
datagram groups ([moq-dev/moq#4982](https://github.com/moq-dev/moq/pull/4982)),
and held fetches ([moq-dev/moq#5623](https://github.com/moq-dev/moq/pull/5623)).
The external interop runner (`englishm/moq-interop-runner`) has no FETCH
case, and our own publishers and subscribers talk moq-lite to each other.
Refusing a FETCH is allowed by the spec. The aim is to implement the simple
parts of moq-transport.

Decision prompts:

- Goal for FETCH: ✅ refuse multi-group; cache-only ranges marked `0x10C`;
  joining only.
- Draft-22 multi-group fills: ✅ same rule as FETCH; keep multi-group fills.
- Who refuses: ✅ only our publishers (the maintainer, correcting an earlier
  "both directions" pick: "subscriber.rs can do a range FETCH but
  publisher.rs will return NOT_SUPPORTED"); both publishers and subscribers.
- A wider fill: ✅ the fill-failure reset, keeping the SUBSCRIBE; refusing
  the whole SUBSCRIBE.
- FILL_TIMEOUT: refuse in both; ✅ accept and ignore in both; narrow the Rust
  quest to one group. One live group ends on its own, so the wait is
  bounded. A zero (cache-only) value may get objects that arrive later,
  which is not quite conforming and is accepted.
- Hole-marker extension: ✅ its own m2 quest,
  [moq-transport hole markers](/quest/m2/ietf-hole-marker.md), which lifts
  this refusal wherever the cache knows every group in the range.
- Docs: ✅ inline only; a separate docs quest.
- Rank: ✅ before the other FETCH quests, right after
  [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md); at m1's end.

Implementation:

- **Rust publisher.** Refuse a standalone FETCH spanning several groups with
  NOT_SUPPORTED, alongside the existing joining-FETCH refusal
  (`a_joining_fetch_spanning_several_groups_is_refused`). Resolve a
  draft-22 fill to `FillServe::Group` for the largest group or
  `FillServe::Unsupported`, deleting `FillServe::Range` and the range half of
  `fill_range`. Accept FILL_TIMEOUT instead of the `"FILL_TIMEOUT not
  supported"` refusal. Delete the end-location, `LOCATION_FILTER`, and
  multi-group ordering rules only the range path needs, with their tests.
- **JS publisher.** Same rules in `#answerFetch` and `#runFill`. Remove the
  FILL_TIMEOUT deadline and its timed-out markers from
  [moq-dev/moq#7051](https://github.com/moq-dev/moq/pull/7051). Keep the End
  of Range encoding, which
  [JS evicted FETCH groups are unknown](/quest/m1/js-fetch-evicted-unknown.md)
  uses for a group lost after FETCH_OK.
- **Tests.** Unit tests in both publishers for each refusal and for each
  kept form on drafts 18 and 22. `warm_backfill` in
  `rs/moq-net/tests/integration` keeps draft 22 out of its backfill and
  instead asserts that a `0..` fill fails while the subscription delivers
  live. This replaces the deleted cold-floor quest from
  [moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168): our publisher
  now refuses that fill on purpose. Narrow `moq fetch` and the interop FETCH
  cells to one group where they asked for more.
- **Compat.** If a released-version cell in `test/interop/compat.sh` breaks
  (the FETCH capability cell, or a cell that fetched a range), add a planned
  break to `test/interop/compat/planned-breaks.json`.
- **Docs.** Rewrite the "History is fetched in ranges" and "Refused, not
  fatal" bullets of `doc/concept/standard.md`.

Run `just check`, `just test interop --all`, and the compat lane.

Public API: none. Wire: our publishers refuse multi-group FETCH with
NOT_SUPPORTED, fail multi-group fills, and ignore FILL_TIMEOUT.

## Related

- [Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) - joining FETCH behavior this keeps
- [JS subscribers FETCH over moq-transport](/quest/m1/interop-coverage/js-ietf-fetch.md) - the JS subscriber's range FETCH, which stays
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - relays fill misses upstream by range, unchanged here
