# [M] Binding subscriptions take a Delay { min, max }

## Goal

Every binding's subscription takes the same `Delay { min, max }` that moq-net's `track::Subscription` and `@moq/net`'s `Track.Subscription` took in [moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597), in place of a bare max delay. That covers FFI `MoqSubscription`, moq-c `moq_subscription`, and the Python, Swift, Kotlin, Go, Dart, and C++ wrappers. An omitted max means 0 in every binding, including Dart, Go, and C.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m1, because it breaks the public API and doing it before the cut avoids a second break.
[moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597) landed the Rust and JS half: `track::Delay` / `Track.Delay`, `Subscription.delay`, the lite-07 `Subscriber Min Age`, and its aggregation. The bindings still expose `max_delay` and map it onto `delay.max`, stating a zero `min`.

- **Representation:** decided 2026-11-02 (maintainer): the shared `Delay` has a nullable minimum. FFI `MoqDelay.min` becomes an explicit `null` (`Option<Duration>`) that defaults to a stated zero, and C and Go get explicit absence flags so a zero-initialized value stays a stated zero. Media consumers read an unstated minimum as zero locally, and subscription metadata keeps it.
- **FFI:** `MoqSubscription.max_delay` becomes `delay: MoqDelay`, the same record the decoders take. `moq_subscription_defaults()` returns moq-net's defaults.
- **Binding defaults:** this change also fixes the bindings where an omitted max is still unlimited.
  - Dart: an optional nullable parameter.
  - Go: a nil `MaxDelay` in a zero `Subscription{}`.
  - C: `max_delay_present` is false in a zeroed struct.

  Each one's omitted or zeroed form becomes 0, and unlimited stays expressible. Dart defaults the parameter to `Duration.zero`, so an explicit `null` stays unlimited. Go's nil and C's unset `present` can't mean both, so each gains an explicit unlimited flag in the delay, as `moq_delay.max_unlimited` already does for decoders; the PR names it.
- Update `doc/lib/{py,swift,kt,go,dart,c,cpp}` and add MIGRATE notes under `## Unreleased` for each breaking binding change.

Public API: breaking in every binding. Wire: none.

## Related

- [A max-delay decoder joins an earlier group](https://github.com/moq-dev/moq/pull/7311) - its PR, #7311, adds `Delay`
- [Ordered reads wait only on owed groups](https://github.com/moq-dev/moq/pull/7403) - the Rust rule; its PR, #7403, settles the max-delay defaults and touches the same fields
