# [S] Lite subscriber polls only routes with a request

## Goal

A moq-lite session's per-group cost stops growing with the number of
broadcasts its peer has announced: delivery over a session holding thousands
of announced routes costs what it costs with a handful, as it already does
over moq-transport.

## Plan

`Announced::poll_serve` (`rs/moq-net/src/lite/subscriber.rs`) polls every
attached route's `Dynamic::poll_requested_broadcast` on every driver wake, so
each incoming group pays for every route, and each pending poll registers the
driver's waiter on every idle route's list. The IETF subscriber runs one task
per route and only wakes the one that got a request.

Measured with `cargo bench -p moq-net --bench session` (2026-09, one relay,
16 viewers each watching one broadcast):

- `session_delivery_broadcasts`: lite 445 µs at 16 announced, 15.2 ms at
  4096; IETF flat around 458 µs. 57% of lite CPU is `WaiterList::register`
  under `poll_requested_broadcast`.
- `session_delivery_scale` at 256 publishers x 256 viewers: lite 40 ms, IETF
  16.5 ms, since every viewer session holds all 256 routes.

Wake only routes with a pending request. A task per route like IETF's or a
ready set both fit; pick by what keeps the driver's fairness rules. Land
with the before/after of those two groups.
