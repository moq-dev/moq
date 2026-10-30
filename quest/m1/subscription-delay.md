# [L] Subscriptions take a Delay { min, max }

## Goal

Every subscription API takes the same `Delay { min, max }` the consumer APIs got in [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311), in place of a bare max delay. That covers moq-net `Subscription`, `@moq/net`'s `Subscription`, FFI `MoqSubscription`, and moq-c `moq_subscription`. The `Delay` type moves to moq-net / `@moq/net`, and an omitted max means 0 in every binding, including Dart, Go, and C.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m1, because it breaks the public API and doing it before the cut avoids a second break.

- **Move:** move `Delay` from `moq_mux::container` / `Container.Delay` into `moq_net::track` / `Track.Delay`. Re-export it from moq-mux for one release with a MIGRATE note.
- **Rename:** rename `max_delay` to `delay.max` throughout, about 1,050 Rust and 600 JS references. On the wire, `max` still travels as today and `min` stays local.
- **Semantics:** they're settled in [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403): every default is 0, unlimited is explicit (Rust `None`, JS `null`, FFI null), and an omitted JS field means 0.
- **Binding defaults:** this change also fixes the bindings where an omitted max is still unlimited.
  - Dart: an optional nullable parameter.
  - Go: a nil `MaxDelay` in a zero `Subscription{}`.
  - C: `max_delay_present` is false in a zeroed struct.

  Each one's omitted or zeroed form becomes 0. The breaking binding changes get MIGRATE notes under `## Unreleased`.

Public API: breaking in every binding. Wire: none.

## Required

- [Ordered reads wait only on owed groups](/quest/m2/ordered-owed-wait.md) - its PR, #7403, settles the max-delay defaults and touches the same fields
- [A max-delay decoder joins an earlier group](/quest/m2/intra-refresh/join-earlier-group.md) - its PR, #7311, adds `Delay`
