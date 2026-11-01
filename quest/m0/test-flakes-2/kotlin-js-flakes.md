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

- **Kotlin.** `kt/moq/src/jvmAndAndroidTest/kotlin/dev/moq/SmokeTest.kt:518`.
  An `unroutable` answer means no route served the path when the request
  resolved. A request made before the publisher's route reached the origin
  returns it, and so does one made after that route was withdrawn or the
  broadcast closed. Check both a wait on announcement that the test skips
  and a route withdrawal or close during the collection it cancels.
- **JS.** `js/net/src/integration.test.ts:1994`, the moq-transport variant
  of the lite test at `:1929`. Under load the late publisher's arrival and
  the blind handle's wait race some deadline; find which, and replace the
  wall-time dependency with an event or a controlled clock.

Follow the epic's rules: no raised timeout and no retry.

Public API: none. Wire: none.
