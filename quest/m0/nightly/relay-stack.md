# [S] The relay's startup fits its stack under all features

## Goal

`moq-relay`'s `stack::debug_relay_load_and_run_fit_windows_stack`
(`rs/moq-relay/tests/integration/stack.rs`) passes under `--all-features`
on `main` instead of overflowing its 1 MiB thread. Fixed at the cause, by
shrinking the future that grew, never by raising the stack size the test
models.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [Nightly passes on main](/quest/m0/nightly/README.md)
- m1
- m2

- Find which feature grows `Relay::load`/`Relay::run`'s future: run the
  test with each feature added to the defaults, then measure the futures'
  sizes (`-Zprint-type-sizes`, or `size_of_val` on the futures in a test).
- Box or split the large state that feature adds, the way the default build
  already fits.
- Keep the test's 1 MiB stack, which stands for a Windows main thread.

Public API: none expected. Wire: none.
