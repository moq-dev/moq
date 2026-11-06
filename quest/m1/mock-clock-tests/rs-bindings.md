# [M] Binding and CLI tests run on a mocked clock

## Goal

The FFI, C, CLI, and `kio` tests that wait on real time move to a paused or
simulated clock, or an awaited event. See the
[epic](/quest/m1/mock-clock-tests/README.md) for the rules.

## Plan

As of 2026-11-06:

- moq-ffi, 10: `src/test.rs` 8, `src/test/rung.rs`, and
  `src/bandwidth.rs`.
- moq-c, at least 6 in `src/test.rs`, including the 50 ms proof in
  `States::recv_terminal`, which the scan undercounted.
- moq-cli, 9: `tests/integration/export.rs` 3, `auth_serve.rs` 2,
  `listen.rs`, `src/archive.rs` 2, and `src/main.rs`.
- kio, 3: `src/clock/sim.rs` 2 and `src/channel.rs`.

Fix each FFI test together with its Python, Go, Swift, and Kotlin siblings
where they exist.

Public API: none. Wire: none.
