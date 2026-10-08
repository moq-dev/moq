# [S] @moq/net per-session caps match moq-net

## Goal

Rust moq-net's per-session caps hold in `@moq/net` too, so a JS peer or
relay refuses past them like a Rust one.

## Plan

- Per-session caps (#4820, landed): add Rust's live announce and
  subscription caps to JS with the same defaults (100,000 and 10,000) and
  close the session with TOO_MANY_REQUESTS (0x7) past them, as Rust does. JS's own request-ID window is
  [JS request window](/quest/m1/js-request-window.md).
Test against the Rust behavior, with mocked time. The pending-tail hold
split out to [JS pending tail](/quest/m1/js-pending-tail.md) on 2026-10-08,
since it waits on #4225 and the caps do not.

Decided 2026-10-08: the resolved-epoch check is done for JS
(`broadcast.Consumer.epoch` mirrors `broadcast::Info::epoch`), and the
bindings' epoch surface is [Bindings](/quest/m0/broadcast-epoch/bindings.md)'s.

## Related

- [JS pending tail](/quest/m1/js-pending-tail.md) - the other half of session parity, behind #4225
- [JS request window](/quest/m1/js-request-window.md) - JS grants request IDs as requests close, the other half of JS request limits
