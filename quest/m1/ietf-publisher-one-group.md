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

Implementation is checkpointed in [moq-dev/moq#7204](https://github.com/moq-dev/moq/pull/7204): both publishers serve one-group FETCH and the largest-group fill, ignore FILL_TIMEOUT, and no longer serve ranges. Subscriber range requests are unchanged. The publisher-only model helpers and timeout machinery are removed; fixtures and benchmarks use the kept forms.

Remaining decision: a refused fill can reset before its FETCH_HEADER is received. Our subscriber cannot associate that stream with its request, so its sole fill slot remains outstanding. `run_subscribe` defers every subsequent widening while that slot is outstanding. The existing `draft22_updates_both_bounds_across_cached_groups` integration test now stalls after `2..4 -> 0..6 -> 4..5 -> 0..`, losing live group 6 even though the subscription remains open. The memory transport's `reset_drops_queued_messages` test and the subscriber's deferred-update comment describe this case.

Recommendation: permit a narrow subscriber fill-ownership fix that lets later updates retire an unidentifiable failed fill without changing the range FETCH or multi-group fill requests it sends. That exceeds the settled publisher-only scope and needs the maintainer's decision. Accepting permanently stalled widening is the alternative, not a test change to conceal it.

After that decision, finish local checks and `just test interop --all`, then remove this quest and its links. The compatibility lane currently stops before session cells because accepted pins name moq-cli 0.14.8 while 0.14.9 is published. [moq-dev/moq#7061](https://github.com/moq-dev/moq/pull/7061) already owns that blocked remeasurement; do not repin it here without new evidence of a break from this change.

Public API: none. Wire: our publishers refuse multi-group FETCH with
NOT_SUPPORTED, fail multi-group fills, and ignore FILL_TIMEOUT.

## Related

- [Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) - joining FETCH behavior this keeps
- [JS subscribers FETCH over moq-transport](/quest/m1/interop-coverage/js-ietf-fetch.md) - the JS subscriber's range FETCH, which stays
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - relays fill misses upstream by range, unchanged here
