# [M] A failed archive time lookup fails the subscription

## Goal

When an `archive::Consumer` cannot answer a `Time(pts)` start because a
store GET fails or an object does not decode, the subscription fails with a
typed error naming the failure instead of silently falling back to the
cache. A time range on a history-served track also counts an unverified
group's start even when its handler registered after the group's first
frame, and moq-ffi plus the bindings expose a fill's `floor`
(`track::FillGroups::floor`), so a binding-side handler can answer a time
start with the group below it.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310) (merged),
which added the time lookup and finished the archive time ranges quest.
[fill-lookups](https://github.com/moq-dev/moq/pull/9228) since made the lookup an
ordinary group fill: a time start's fill is one key wide from the time, and
`FillGroups::floor` says how far below it the group containing the time may
start.

- What a lookup's storage or decode failure does:
  - Silent cache fallback, as [moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310) documents with a warning.
  - ✅ Refuse + report. The archive already refuses a time start's fill
    when the store fails (`FillGroups::refuse_groups`); make that refusal
    fail the reader's span, so the subscription ends with a typed error,
    instead of falling back to the groups cached. No retry here: the store
    client already retries. Reason: a cache fallback silently serves the
    wrong range, which a DVR seek cannot tell from success.
  - Retry the lookup in moq-net.

Also, from [moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310)'s follow-ups:

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

Tests: a failing GET and a malformed object each fail the subscription with
the typed error; a handler registered after a group started still resolves
a time range over it; an FFI handler answers a lookup.

Public API: a refused time start fails its reader; moq-ffi's fill gains
`floor()`. Wire: none.

## Related

- [DVR rewind](/quest/m4/archive-dvr.md) - the consumer of time seeks
