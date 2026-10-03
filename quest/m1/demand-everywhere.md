# [M] Demand is the one way to watch subscribers

## Goal

Every model handle that serves subscribers watches them through a
`Demand` handle returned by `demand()`: `track::Request`, `group::Request`,
`group::Producer`, and `broadcast::Producer`. Their own
`used`/`unused`/`is_used`/`poll_unused` copies are gone, in Rust and `js/net`.
moq-ffi then exposes the same handle on its request and producer types through
the [FFI shape](/quest/m1/ffi-shape/README.md) line, so a binding's fetch
handler can see a group request abandoned.

## Plan

Facts (2026-10-01): `track::Demand` and `broadcast::Demand` already exist.
[#4528](https://github.com/moq-dev/moq/pull/4528) (the track-demand quest) moved
`track::Producer` to `demand()`, and deliberately kept group and
broadcast `used`/`unused`, since group demand drives fetch coalescing.
`broadcast::Producer` is already `demand()`-only, with `used`/`unused` on
`broadcast::Demand`; there is no `group::Demand` yet.
`track::Request` and `track::Dynamic` have `poll_unused`, and #4691 added
`group::Request::poll_unused`. moq-ffi wraps only `track::Demand`
(`MoqTrackDemand`); `MoqGroupRequest` and `MoqTrackRequest` have no demand.

Decided (2026-10-01):

- ✅ Everything under `Demand`, reversing track-demand's call to keep group and
  broadcast `used`/`unused`. Rejected: Requests only.
- ✅ The binding exposure follows this quest in the FFI shape line instead of
  wrapping `poll_unused` now. Rejected: a standalone moq-ffi quest for
  `poll_unused`, and waiting in m2 for a consumer.
- ✅ m1, ahead of FFI shape, so the bindings expose `Demand` once.

- ✅ One type per level (`track::Demand`, `group::Demand`,
  `broadcast::Demand`) with the same method set, like the two that exist.
  Rejected: one generic `Demand`, which erases level-specific state such as
  track priority.

Keep fetch coalescing's group demand semantics, and keep `abort_unused` where
its race still needs an owner.

Public API: breaking in moq-net and `@moq/net`. Wire: none.

## Related

- [IETF FETCH abandonment](/quest/m1/ietf-fetch-abandonment.md) - first new consumer of group request demand; can ship on `poll_unused` without waiting for this
