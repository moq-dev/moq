# [M] @moq/net session parity with moq-net

## Goal

Three behaviors that landed in Rust on 2026-10-07 hold in `@moq/net` too, so a
JS peer or relay behaves like a Rust one.

## Plan

- Per-session caps (#4820): JS has no live announce or subscription caps.
  Add them with the same defaults (100,000 and 10,000) and close the session
  with TOO_MANY_REQUESTS (0x7) past them, as Rust does.
- Pending tail (#4225): Rust readers of a received track wait for missing
  groups below a declared end until the tail settles; JS readers still end at
  the end with groups missing (`doc/lib/js/net.md`). Mirror the hold.
- Resolved epoch (#4904, #4967): Rust `broadcast::Consumer::epoch()` and
  `broadcast::Info::epoch` carry the epoch of the route a request resolved
  through; JS has `Consumer.epoch` from #4904, so check `Info` parity and the
  bindings (moq-ffi) surface the same value.

Test each against the Rust behavior, with mocked time.
