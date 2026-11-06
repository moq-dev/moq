# [S] Go and Python tests run on a mocked clock

## Goal

The Go wrapper's and Python packages' tests stop proving nothing happened by
waiting, and stop sleeping to settle. See the
[epic](/quest/m1/mock-clock-tests/README.md) for the rules.

## Plan

As of 2026-11-06:

- Go: `go/wrapper/moq_test.go:1477` (`untilWanted` must hit a 50 ms
  deadline), `:1283` (a 5 ms context), the poll sleeps at `:1570` and
  `reconnect_test.go:217`, and `internal/bridge/call_test.go:87`. Go has no
  fake clock yet; prefer awaiting the event over adding one.
- Python: `py/moq-rs/tests/test_local.py:137`, `:1482`, and `:1516` each
  sleep, then assert a task isn't done. `moq.testing.Clock` exists, but only
  its own tests use it. Also `py/moq-ffi/tests/exit_inside_callback.py:49`
  and `demo/carla/sim/tests/test_world.py:960`, `:969`.

Public API: none. Wire: none.
