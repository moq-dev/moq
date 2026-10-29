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

The implementation is complete in [the draft PR](https://github.com/moq-dev/moq/pull/7258).
Remaining work is local validation: run `quest gate just ci` on a host where
the Cargo launcher can execute the selected commands and the Nix builder can
create user namespaces for the runner-image check. This host refused both
(`mbx`: `Argument list too long`; runner image: `/proc/self/uid_map` denied).
The affected crates pass `just rs check`, including all targets and docs.
The congestion, QUIC, and Tokio suites pass all 1,297 tests. io_uring's suite
also needs enough user-wide memlock: a serial run passed 80 tests but failed
49 during ring setup at this host's shared 8 MiB limit. Local review found no
remaining implementation issue. Finish the full gate and io_uring tests,
review any resulting fixes, then remove this quest and its links and mark
the PR ready. Do not merge or enqueue it.

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
- **Remove `into_any`**: selection tests use observable behavior and metrics.
  QUIC's duplicate inspection of ProbeRTT internals and the test-only BBR3
  accessors are removed; the direct controller tests cover those model
  invariants, and QUIC retains the media deadline check. noq's adapter keeps
  its own `into_any` on its side of the boundary.
- moq-quic's `transport::Config` already nests the controller factory
  (`congestion: Arc<dyn congestion::Factory + ...>`, defaulting to
  `congestion::bbr3::Config`); its uses, tests, and bench use the new names.

Public API: breaking in moq-congestion (unpublished) and in the moq-quic,
moq-tokio, and moq-uring re-exports or signatures that name these types.
Wire: none.
