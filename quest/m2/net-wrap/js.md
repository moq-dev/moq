# [L] JS mirror of the wrapper traits

## Goal

`@moq/net` exports interfaces with the Rust traits' names and methods, and
`@moq/hang`, `@moq/publish`, and `@moq/watch` accept them where they take a
concrete broadcast, track, or group today. `@moq/e2ee`'s wrappers implement
them, so a browser publisher or player handles an encrypted broadcast without
its own decryption path.

## Plan

- Mirror the [Rust traits](/quest/m2/net-wrap/traits.md) one to one, per the
  cross-language naming rule. TypeScript's structural types make the default
  free; there is no dispatch question.
- Take the interfaces only where a wrapper must pass through, as needed:
  `@moq/hang`'s catalog and container code, `@moq/publish`'s broadcast,
  and `@moq/watch`'s subscriptions.
- `@moq/e2ee`'s broadcast, track, and group wrappers adapt to `@moq/net`'s
  method shapes, as moq-e2ee does in Rust, with its errors mapped to
  `@moq/net`'s. Like moq-e2ee, it compresses a `compress` track before
  encryption and publishes the physical track uncompressed.
- Demand and preference cross the wrapper: a publisher with no viewer
  through an encrypted track stays idle.
- Test publish and watch round trips through a pass-through wrapper and
  through `@moq/e2ee`.

Public API: new `@moq/net` interfaces; `@moq/e2ee` wrapper signatures change.
Wire: none.

## Required

- [moq-net traits](/quest/m2/net-wrap/traits.md) - the names and methods this mirrors
