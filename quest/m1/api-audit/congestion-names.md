# [S] moq-congestion's first-publish names

## Goal

`moq-congestion` first publishes with namespaced names and no downcasting:
`bbr3::Config`, `cubic::Config`, `Metrics`, and `Factory`, with `Bbr3`,
`Cubic`, and `Unlimited` at the crate root, and no `Controller::into_any`.
Every caller in moq-quic, moq-tokio, moq-uring, and the benches uses the new
names.

Non-goals: new controller signals or a controller framework, and
`clone_box`, which QUIC path migration needs.

## Plan

Decided 2026-10-27 from the release-to-main API audit (maintainer picked each
option):

- The crate is still 0.1.0 and unpublished, and
  [noq adopts moq-congestion](/quest/m2/noq-congestion.md) starts after the
  m1 cut publishes it, so the rename costs one change now and nothing later.
- **Namespace** (chosen over keeping the quinn-style names that noq's
  adapter would read more easily): make `bbr3` and `cubic` public modules
  exporting `Config`. Rename `ControllerMetrics` to `Metrics` and
  `ControllerFactory` to `Factory`. Keep `Bbr3`, `Cubic`, `Unlimited`,
  `Controller`, `Packet`, and the event structs at the root.
- **Remove `into_any`**: its only callers are the moq-tokio tests in
  `rs/moq-tokio/src/quic/backend.rs` that assert which controller a factory
  built. Check the selection another way (such as observable behavior or
  metrics) or drop an assertion that only restates the `match`. noq's adapter
  keeps its own `into_any` on its side of the boundary.
- moq-quic's `transport::Config` already nests the controller factory
  (`congestion: Arc<dyn congestion::ControllerFactory + ...>`, defaulting to
  `congestion::Bbr3Config`); rename its uses, its tests, and its bench to the
  new names here.

Public API: breaking in moq-congestion (unpublished) and in the moq-quic,
moq-tokio, and moq-uring re-exports or signatures that name these types.
Wire: none.
