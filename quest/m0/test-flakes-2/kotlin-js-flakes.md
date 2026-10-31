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

- **Kotlin.** `kt/moq/src/jvmAndAndroidTest/kotlin/dev/moq/SmokeTest.kt:462`.
  An `unroutable` answer means the request resolved before the publisher's
  route reached the origin, so look for a wait on announcement that the test
  skips or a race between the publish and the collection it cancels.
- **JS.** `js/net/src/integration.test.ts:1998`, the moq-transport variant
  of the lite test at `:1933`. Under load the late publisher's arrival and
  the blind handle's wait race some deadline; find which, and replace the
  wall-time dependency with an event or a controlled clock.

Follow the epic's rules: no raised timeout and no retry.

Public API: none. Wire: none.
