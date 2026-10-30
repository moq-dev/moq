# [S] OBS TSan passes without a wall-clock bound

## Goal

Nightly's OBS TSan job passes, and `destroySource` in
`cpp/obs/test/moq-source-test.cpp` no longer asserts elapsed wall time.

## Plan

Nightly [40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673) (2026-10-27, at 22808bbb2f) reported zero ThreadSanitizer
diagnostics. The only failure is `elapsed < 500ms` at
`moq-source-test.cpp:553` in the connect-failure case, and TSan's slowdown
is enough to break it. Per AGENTS.md, tests do not depend on wall-clock
timing: assert the ordering the bound stood in for (destroy returns after the
connect attempt is cancelled, not after it times out) through an event or a
mocked clock.

Public API: none. Wire: none.
