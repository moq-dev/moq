# [L] Subscriptions take a Delay { min, max }

## Goal

Every subscription API takes the same `Delay { min, max }` the consumer APIs got in [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311), in place of a bare max delay. That covers moq-net `Subscription`, `@moq/net`'s `Subscription`, FFI `MoqSubscription`, and moq-c `moq_subscription`. The `Delay` type moves to moq-net / `@moq/net`, and an omitted max means 0 in every binding, including Dart, Go, and C.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m1, because it breaks the public API and doing it before the cut avoids a second break.

- **Move:** move `Delay` from `moq_mux::container` / `Container.Delay` into `moq_net::track` / `Track.Delay`. Update moq-mux's callers directly and leave no re-export, since `AGENTS.md` forbids compatibility shims; the MIGRATE note names the new path.
- **Rename:** rename `max_delay` to `delay.max` throughout, about 1,050 Rust and 600 JS references. `max` travels on the wire as today.
- **`min` on the wire:** decided 2026-10-30 (maintainer): because `Delay` lives in moq-net, `min` goes on the lite-07 SUBSCRIBE (and SUBSCRIBE_UPDATE) too, following the rule that everything in moq-net survives the wire. Had `Delay` stayed in moq-mux, it would have stayed local. A relay forwards a subscriber's `min` upstream, and merged subscriptions take the smallest. Nothing reads it on the publisher side yet; later uses could include starting a cold join server-side, delay-tolerant batching, and priority under congestion. Versions that can't carry `min` (lite-01..06, moq-transport) follow the `carries_max_delay` precedent (`rs/moq-net/src/lite/subscribe.rs:113`): encoding to them drops `min`, and decoding from them yields "not stated", not 0. A relay forwards a not-stated `min` as not stated, and it doesn't count toward a merged subscription's smallest, so a legacy peer doesn't pull a merged `min` to 0.
- **Semantics:** they're settled in [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403): every default is 0, unlimited is explicit (Rust `None`, JS `null`, FFI null), and an omitted JS field means 0.
- **Binding defaults:** this change also fixes the bindings where an omitted max is still unlimited.
  - Dart: an optional nullable parameter.
  - Go: a nil `MaxDelay` in a zero `Subscription{}`.
  - C: `max_delay_present` is false in a zeroed struct.

  Each one's omitted or zeroed form becomes 0, and unlimited stays expressible. Dart defaults the parameter to `Duration.zero`, so an explicit `null` stays unlimited. Go's nil and C's unset `present` can't mean both, so each gains an explicit unlimited flag in the delay; the PR names it. The breaking binding changes get MIGRATE notes under `## Unreleased`.

Public API: breaking in every binding. Wire: lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE gain `min`.

## Required

- [A max-delay decoder joins an earlier group](/quest/m2/intra-refresh/join-earlier-group.md) - its PR, #7311, adds `Delay`

## Related

- [Ordered reads wait only on owed groups](https://github.com/moq-dev/moq/pull/7403) - the Rust rule; its PR, #7403, settles the max-delay defaults and touches the same fields
