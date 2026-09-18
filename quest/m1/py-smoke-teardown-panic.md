# [M] Python subscriber teardown never panics the moq-ffi thread

## Goal

The `python -> python` smoke pair passes every run. About one run in four,
after the subscriber has received every byte, the moq-ffi thread dies with
"panic in a function that cannot unwind" while the Python subscriber tears
down. A panic across the FFI boundary aborts the host process, which is
user-visible breakage on a released binding, so it is fixed before the
release rather than retried.

## Plan

Branch from dev, where the smoke harness and the Python wrapper live in
their current shape.

- Reproduce first: loop `just test smoke` on the python pair with
  `RUST_BACKTRACE=1` and the run directory kept (`MOQ_TEST_RUNS`), and
  record the backtrace in the PR. The harness deletes its run directory on
  teardown, so no log of the original failure survives.
- The panic site is a `extern "C"` or uniffi callback frame that cannot
  unwind: find which drop or callback runs during subscriber teardown after
  the session has closed (a `Drop` touching a runtime that is already shut
  down, a callback after `user_data` was released, or a double close from
  Python's finalizer racing the explicit close).
- Fix it at the source: the operation is made safe after shutdown, or the
  order that made it unsafe is enforced by the types. No `catch_unwind`
  papering over it.
- Land a regression test at the level the cause lives: a moq-ffi unit test
  when the drop order is the cause, otherwise a Python test in `py/` that
  closes a subscriber in the racing order.

Public API: none. Wire: none.
