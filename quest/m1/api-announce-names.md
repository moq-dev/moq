# [M] Announce, request, and origin config names mirror across every language

## Goal

One name per concept on the announce and request surface, in Rust, JS,
moq-ffi, and the C ABI. Today the announce event is `announce::Update` in
Rust, `Announce.Event` in JS, and `MoqAnnouncement` in moq-ffi, whose
consumer is `MoqAnnounced` and whose `path()` returns a pattern; refusing a
broadcast request is `reject()` in Rust and JS but `abort(code)` in moq-ffi
and `moq_broadcast_request_abort` in C, where `abort` already means killing
a producer; `origin::Requesting` sits beside `origin::Request` for the
opposite role; and the origin's configuration is `origin::Info` in Rust and
`MoqOriginOptions` in the bindings while `track::Info` is metadata.

## Plan

Decided 2026-09-14: `Update` wins, `reject` wins, `Info` becomes `Config`,
and `Dynamic` stays.

- JS `Announce.Event` becomes `Announce.Update` (`js/net/src/announced.ts`).
- moq-ffi `MoqAnnounced` becomes `MoqAnnounceConsumer`, `MoqAnnouncement`
  becomes `MoqAnnounceUpdate`, and its `path()` becomes `pattern()`
  (`rs/moq-ffi/src/origin.rs`); the C ABI and every wrapper follow through
  the Cross-Package Sync row.
- `MoqBroadcastRequest::abort` becomes `reject` and
  `moq_broadcast_request_abort` becomes `moq_broadcast_request_reject`;
  `moq_broadcast_request_free` stays `_free`, the spelling every other
  one-shot payload handle uses (`rs/libmoq/src/api.rs`).
- `origin::Requesting` (`rs/moq-net/src/model/origin.rs`) becomes
  `origin::Pending`, the consumer-side wait for a request to resolve.
- `origin::Info` becomes `origin::Config` with its `with_*` builders
  replaced by public fields, and `MoqOriginOptions` becomes
  `MoqOriginConfig`; JS follows if it exposes the same record.

Public API: breaking on moq-net, @moq/net, moq-ffi, libmoq, and every
binding, so on dev. Wire: none. Run `just test smoke-full`.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so the announce surface ships under one name
- [Rate estimate names](/quest/m1/api-rate-estimate-names.md) - the same rule applied to the stats surface
