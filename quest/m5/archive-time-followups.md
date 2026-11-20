# [M] Archive time ranges: late handlers, binding floors, and time ends

## Goal

A time range on a history-served track counts an unverified group's start
even when its handler registered after the group's first frame, moq-ffi plus
the bindings expose a fill's `floor` (`track::FillGroups::floor`), so a
binding-side handler can answer a time start with the group below it, and a
long time end's cost against the history is measured.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310) (merged),
which added the time lookup and finished the archive time ranges quest.
[fill-lookups](https://github.com/moq-dev/moq/pull/9228) since made the lookup an
ordinary group fill: a time start's fill is one key wide from the time, and
`FillGroups::floor` says how far below it the group containing the time may
start. That PR also made a refused time start fail its reader with the
handler's error, and a dropped one start below its floor, never from what the
cache holds, so a store failure in `archive::Consumer` already fails the
subscription.

From [moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310)'s follow-ups:

- Unverified starts are indexed when a group's first frame is written,
  using whether a history-serving handler exists then. A handler registered
  later misses those groups, so index them (or re-check) when the handler
  registers.
- Mirror `track::FillGroups::floor` in `rs/moq-ffi` and moq-c (whose
  handlers serve a time start's key like any other today) and the binding
  wrappers, so a binding-side history handler can answer a time start with
  the group below it.
- A time end walks the history one fill round trip per group. Measure a
  long range against the in-memory store; if DVR seeks need it, look the
  end up as well. Otherwise record the cost in the `FillGroups::floor` docs.

Tests: a handler registered after a group started still resolves a time
range over it; an FFI handler answers a lookup.

Public API: moq-ffi's fill gains `floor()`. Wire: none.

## Related

- [DVR rewind](/quest/m4/archive-dvr.md) - the consumer of time seeks
