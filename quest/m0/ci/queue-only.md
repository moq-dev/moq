# [S] Cloud CI runs only in the merge queue

## Goal

A PR into `main` runs no cloud Check, Test or optional workflow. Its required
contexts report skipped, so it can be enqueued as soon as its review is
clean. Contributors and agents run `just ci` locally before pushing, and the
merge queue is the only cloud gate. PRs into `release` keep running
everything, since release has no queue.

## Plan

Decisions (maintainer, 2026-10-19):

- Every PR into `main`, forks included, skips cloud CI. A contributor gets no
  CI until a maintainer enqueues the PR; CONTRIBUTING.md tells them to run
  `just ci` first. Rejected: keeping PR CI for forks, or an opt-in label.
- Nothing records a local pass. The queue is the gate, and a skipped local
  run only costs a queue ejection, not a bad merge. Rejected: a `local-ci`
  commit status, which is easy to forge and adds machinery.
- `just ci` with no arguments runs exactly what the queue's Linux gate runs:
  the check and test halves, `MOQ_STRICT=1` and the `ci` nextest profile,
  diffed against `origin/main`. The queue jobs call the same recipe, so the
  local and cloud runs cannot drift. `just check` stays the lenient dev loop.
- The optional workflows that run on main PRs today (ts, interop,
  release-dart-ffi, release-go-ffi, release-kt-lib, release-swift-lib, obs,
  uring, x11, cpp, wasm, android, video, audio-quality and the rest) move to
  nightly on `main` and keep running on PRs into `release`. That was about
  200 runner-minutes per main PR (measured 2026-10-19). Nightly failures
  already reach Discord through `alert.yml`. `platform.yml` (macOS and
  Windows) leaves main's merge queue as well, superseding the 2026-10-14
  decision; Nightly already runs it. Agents still run
  `just test interop --all` locally for wire changes (AGENTS.md).
- The main queue keeps building at most 5 entries.

Implementation:

- Use the trick `platform.yml` already uses: a job that always runs and
  reports the required name, doing the work only on `merge_group` and on PRs
  whose base is `release`. A PR retargeted from `main` to `release` has to
  run them (`edited`, as `platform.yml` does).
- The quest skills only wait for the checks GitHub reports, so skipped
  contexts satisfy them; no upstream change to kixelated/quest is needed.
- Update CONTRIBUTING.md (`# Merge queue` and the "local `just check`
  passes" line) to say: run `just ci` before marking ready and before every
  push, and enqueue once the review is clean.
- Measured 2026-10-19: PR Check and Test take 13 minutes median and 26
  minutes p90 of wall time, and the queue runs them again, so this halves the
  cloud runs per merge. Record the before and after runner minutes per merged
  PR in the PR.

## Related

- [One CI pass](/quest/m0/ci/one-pass.md) - folds `just ci`'s halves into one queue job
- [Release gates](/quest/m0/ci/release-gates.md) - the ruleset side of the release PR requirement
- [Share local check capacity](/quest/m0/ci/local-gate.md) - local `just ci` runs now replace PR CI, so admission matters more
