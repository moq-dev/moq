# [M] @moq/net session parity with moq-net

## Goal

Three Rust moq-net session behaviors hold in `@moq/net` too, so a JS peer or
relay behaves like a Rust one.

## Plan

- Per-session caps (#4820, landed): add Rust's live announce and
  subscription caps to JS with the same defaults (100,000 and 10,000) and
  close the session with TOO_MANY_REQUESTS (0x7) past them, as Rust does. JS already grants the
  request-ID window (#4966) and refuses past it with the same code.
- Pending tail (#4225, still open): once Rust readers of a received track
  wait for missing groups below a declared end until the tail settles, mirror
  the hold. JS readers still end at the end with groups missing
  (`doc/lib/js/net.md`).
- Resolved epoch (#4904 and #4967, landed): Rust `broadcast::Info::epoch`
  carries the epoch of the route a request resolved through. Check that JS
  (`broadcast.Consumer.epoch`) and the bindings (moq-ffi) surface the same
  value.

Test each against the Rust behavior, with mocked time.
