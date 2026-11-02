# [S] A Kotlin cancel test and a JS late-publisher test hold up

## Goal

Two intermittent failures are fixed at their cause: the Kotlin test
"cancelling a flow collection cancels its native handle" no longer fails
with `unroutable`, and the JS integration test "ietf blind handle picks up a
publisher that arrives late" passes under a heavily loaded `just check`.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session):
root-cause both in one quest, since each is small and they surfaced
together in that session's merge runs.

Checkpoint 2026-11-02 in [moq-dev/moq#7622](https://github.com/moq-dev/moq/pull/7622):

- **Kotlin:** the test consumes a local broadcast, with no origin route or announcement.
  One temporary `System.gc()` before the flow read reproduces `unroutable`: the last
  use of its snapshot producer has passed, so UniFFI cleanup aborts the track.
  Keeping the producer in a `use` scope passes the same injected probe and retains
  ownership through collection. The PR fixes the four snapshot smoke tests with
  this lifetime gap. Demand changes remain event-driven.
- **JS:** a temporary manual-clock jump of 10 seconds when the second SUBSCRIBE
  reaches the publisher reproduces `ControlTimeout` (49). Both late-publisher
  fixtures now install `Manual` without advancing time. They still require a
  specific missing-publisher refusal followed by a successful read on the same
  blind handle. The full integration file passes 20 repetitions (1,960 tests)
  before and after; the controlled probe establishes the setup deadline dependency,
  not the exact scheduler ordering of the historical loaded run.
- **Validation remaining:** a standard `quest gate nix develop --command just ci
  origin/main` must pass before this PR is ready. Normal Kotlin validation fails
  in `mbx` with E2BIG. A diagnostic invocation with `MBX_DISABLE=1` reaches a
  separate Gradle launcher recursion: the Nix shell exports a `gradle` function,
  `command -v gradle` returns `gradle`, and `sh/kt/check.sh` shadows that name with
  its own wrapper, which calls itself. Diagnostic runs with `MBX_DISABLE=1` and
  `GRADLE_CMD=$(type -P gradle)` pass the Kotlin tests (47 wrapper, 5 raw bindings)
  and JS lint/build/tests, but do not count as a standard full-gate pass.
  Tooling owners and the foreground decide the next step. No tooling change or
  upstream post belongs to this PR. Keep it draft until standard validation clears.
- **Overlap inspection:** the open route/serve and subscription-delay PRs
  [moq-dev/moq#7485](https://github.com/moq-dev/moq/pull/7485) and
  [moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597) change different blocks
  in `js/net/src/integration.test.ts`. Related origin API and FFI lifecycle work
  remains with its owners. This PR changes fixtures only.

Follow the epic's rules: no raised timeout and no retry.

Public API: none. Wire: none.
