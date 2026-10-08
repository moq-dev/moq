# [M] @moq/net session parity with moq-net

## Goal

Three Rust moq-net session behaviors hold in `@moq/net` too, so a JS peer or
relay behaves like a Rust one.

## Plan

- Per-session caps (#4820, still open): once Rust ships live announce and
  subscription caps, add them to JS with the same defaults (100,000 and
  10,000 in #4820 today) and close the session with TOO_MANY_REQUESTS (0x7)
  past them, as Rust does. JS's own request-ID window is
  [JS request window](/quest/m1/js-request-window.md).
- Pending tail (#4225, still open): once Rust readers of a received track
  wait for missing groups below a declared end until the tail settles, mirror
  the hold. JS readers still end at the end with groups missing
  (`doc/lib/js/net.md`).
- Resolved epoch (#4904 and #4967, landed): Rust `broadcast::Info::epoch`
  carries the epoch of the route a request resolved through. Check that JS
  (`broadcast.Consumer.epoch`) and the bindings (moq-ffi) surface the same
  value.

Test each against the Rust behavior, with mocked time.

## Required

- [Request caps](/quest/m0/request-caps.md) - #4820's per-session caps land in Rust first

## Related

- [JS request window](/quest/m1/js-request-window.md) - JS grants request IDs as requests close, the other half of JS request limits
