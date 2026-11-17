# [M] A failed archive time lookup fails the subscription

## Goal

When an `archive::Consumer` cannot answer a `Time(pts)` lookup because a
store GET fails or an object does not decode, the subscription fails with a
typed error naming the lookup failure instead of silently falling back to
the cache. A time range on a history-served track also counts an unverified
group's start even when its handler registered after the group's first
frame, and moq-ffi plus the bindings expose time lookups (`track::FillLookup`).

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310) (merged),
which added `track::Fill::time()` and the lookup and finished the archive
time ranges quest.

- What a lookup's storage or decode failure does:
  - Silent cache fallback, as [moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310) documents with a warning.
  - ✅ Refuse + report. Add a lookup refusal to `FillLookup` (for example
    `FillLookup::refuse(err)`); the reader's span fails
    and the subscription ends with a typed error. No retry here: the store
    client already retries. Reason: a cache fallback silently serves the
    wrong range, which a DVR seek cannot tell from success.
  - Retry the lookup in moq-net.

Also, from [moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310)'s follow-ups:

- Unverified starts are indexed when a group's first frame is written,
  using whether a history-serving handler exists then. A handler registered
  later misses those groups, so index them (or re-check) when the handler
  registers.
- Mirror `track::FillLookup` and the new refusal in `rs/moq-ffi` and moq-c
  (whose `requested_fill` and fill handler drop lookups unanswered today) and
  the binding wrappers, so a binding-side history handler can answer lookups.
- A time end walks the history one fill round trip per group. Measure a
  long range against the in-memory store; if DVR seeks need it, look the
  end up as well. Otherwise record the cost in the `FillLookup` docs.

Tests: a failing GET and a malformed object each fail the subscription with
the typed error; a handler registered after a group started still resolves
a time range over it; an FFI handler answers a lookup.

Public API: `track::FillLookup` gains a refusal; moq-ffi gains a lookup
handle with `time()` and the refusal. Wire: none.

## Related

- [DVR rewind](/quest/m4/archive-dvr.md) - the consumer of time seeks
