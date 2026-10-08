# [M] @moq/net session parity with moq-net

## Goal

Two Rust moq-net session behaviors hold in `@moq/net` too, so a JS peer or
relay behaves like a Rust one.

## Plan

- Per-session caps (#4820, landed): add Rust's live announce and
  subscription caps to JS with the same defaults (100,000 and 10,000) and
  close the session with TOO_MANY_REQUESTS (0x7) past them, as Rust does. JS's own request-ID window is
  [JS request window](/quest/m1/js-request-window.md).
- Pending tail (#4225, still open): once Rust readers of a received track
  wait for missing groups below a declared end until the tail settles, mirror
  the hold. JS readers still end at the end with groups missing
  (`doc/lib/js/net.md`).

Test each against the Rust behavior, with mocked time.

Decided 2026-10-08: the resolved-epoch check is done for JS
(`broadcast.Consumer.epoch` mirrors `broadcast::Info::epoch`), and the
bindings' epoch surface is [Bindings](/quest/m0/broadcast-epoch/bindings.md)'s.

## Related

- [JS request window](/quest/m1/js-request-window.md) - JS grants request IDs as requests close, the other half of JS request limits
