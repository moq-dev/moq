# [M] macOS and Windows run in the merge queue

## Goal

No macOS or Windows runner starts for a `pull_request` event. Each merge-queue
entry runs at most one macOS job and one Windows job, which cover what
Platform, OBS, Swift, the moq.sh installer test, and the C++ Windows leg check
on PRs today, plus the
`rs macos-test` and `rs windows-test` suites that now run only nightly. A
platform break still blocks the merge.

## Plan

- The merge queue builds up to five entries at once and macOS is capped at
  five concurrent jobs, so one macOS job per entry is what keeps the queue
  from waiting on runners. Share one target directory across the checks
  inside it. Nightly and release macOS jobs share the cap; measure how often
  they overlap the queue before reserving capacity or lowering the queue's
  build limit.
- `merge_group` has no path filters. A cheap Linux job can ask the existing
  dispatcher whether the merge touches anything platform code compiles, and
  the OS jobs skip otherwise. A skipped job satisfies a required check.
- To gate merges the OS jobs must be required checks in ruleset 2420853, and a
  required check must also report on the PR. Keep the workflows triggered on
  `pull_request` with the jobs skipped there, rather than path-filtered, or
  PRs wait forever. Changing the ruleset is a maintainer action after the
  workflow lands.
- Drop the macOS and Windows legs of PR-time release dry runs only where
  Nightly already runs that workflow (Release Dart FFI, via
  `release-ffi.yml`). Release Swift Lib's macOS jobs have no nightly run, so
  add one before removing them. Release Dart and Kotlin Lib build on Linux
  and stay.
- Linux-only legs of these workflows (OBS on ARM, Android, WASM) are out of
  scope and stay on PRs.
- Check the `release` ruleset too; a `release` PR should not lose platform
  coverage.

## Related

- [mbx on R2](/quest/m0/ci/mbx-r2.md) - gives these jobs a Rust cache, which the Actions cache budget never allowed
