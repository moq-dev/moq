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

Non-goals: changing our subscribers' range requests. `rs/moq-net/src/ietf/subscriber.rs` keeps sending
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

AFK scope choice on 2026-10-29: ✅ adopt the draft's recommended narrow
subscriber fill-ownership fix; accept permanently stalled widening was rejected.
Keep subscriber range FETCH and multi-group fill requests unchanged. A reset may
discard FETCH_HEADER, so later widening must be able to retire a fill that has
no identified stream without allowing its late header to claim a newer fill.

Reproduced before code changes at `5b2485fb4749dbed31e13eb9229b694d3956a5a7`:
`just rs test -p moq-net --test integration draft22_updates_both_bounds_across_cached_groups`
fails at the final live group assertion (`None` instead of `Some(6)`,
`relay=false`). The fill slot remains outstanding after the refused stream's
header is discarded.

The subscriber fix is implemented: widening atomically retires an unidentified
pending fill and reserves request-owned state. Active or ready heads retain their
ownership until handoff. Delayed initial headers and retired update headers
cannot claim a newer fill, including handlers already waiting for an answer.
The unchanged widening integration case now passes. All 3,139 enabled moq-net
tests pass (12 skipped), including the new delayed-header ownership case.

Remaining: finish feasible local CI and `just test interop --all`. The initial
interop run passed all nine Rust/JS protocol tests, then hit the local Cargo
cache wrapper's `Argument list too long` error while building client binaries.
A run using the same pinned Nix toolchain directly is queued through `quest gate`.
Compatibility pin remeasurement remains owned by
[moq-dev/moq#7061](https://github.com/moq-dev/moq/pull/7061); do not repin here.

Public API: none. Wire: our publishers refuse multi-group FETCH with
NOT_SUPPORTED, fail multi-group fills, and ignore FILL_TIMEOUT.

## Related

- [Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) - joining FETCH behavior this keeps
- [JS subscribers FETCH over moq-transport](/quest/m1/interop-coverage/js-ietf-fetch.md) - the JS subscriber's range FETCH, which stays
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - relays fill misses upstream by range, unchanged here
