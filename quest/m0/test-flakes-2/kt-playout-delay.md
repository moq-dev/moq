# [S] Kotlin's playout-delay smoke test holds up under load

## Goal

`audio consumer reports its playout delay` in
`kt/moq/src/jvmAndAndroidTest/kotlin/dev/moq/SmokeTest.kt` passes reliably
under a loaded `just check`, fixed at its cause, and its `@Ignore` is
removed.

## Plan

Planned 2026-11-10 (maintainer, /quest-complete), under the repository's
flaky-test rule: ✅ quarantine now and investigate here (rejected: wait for
a CI failure). While iterating
[moq-dev/moq#8395](https://github.com/moq-dev/moq/pull/8395), which does not
touch it, the test failed once with a coroutine timeout in a full
`just check`, then passed on the next two runs.

The test asserts that a fixed playout delay is reported back as is and that
an estimated one moves as frames are read. Under `runTest`, a timeout means
the test waited on wall-clock progress from the native side (frames read,
the estimate moving) past the coroutine test's budget. Reproduce under load
first; then wait on the event the assertion needs instead of a deadline, as
the epic's rules say, rather than raising the timeout.

Public API: none. Wire: none.
