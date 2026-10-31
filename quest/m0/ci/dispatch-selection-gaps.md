# [S] Dispatcher tool changes run the dispatcher tests

## Goal

A change to the dispatcher's tool lists (`sh/dispatch.sh` and the files it
reads them from) runs `sh/dispatch-query.test.py` in CI, so a tool added to a
module can't break that suite on `main` unnoticed. `sh/rs/uring.test.py`
runs without the repeated `jq: error ... null (null) cannot be matched`
lines.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), from
[moq-dev/moq#7432](https://github.com/moq-dev/moq/pull/7432).

- **Selection.** [6633d2f2e2](https://github.com/moq-dev/moq/commit/6633d2f2e2)
  added `zizmor` to the `gh` module's tools in `sh/dispatch.sh`, and every
  `CI_STRICT` case of `sh/dispatch-query.test.py` then failed with "these
  tools are missing: zizmor" on `main`; #7432 fixes the stub. `run_lane` in
  `sh/dispatch.sh` says gh's tests run on every `check` diff, and a change to
  `sh/dispatch.sh` selects everything, so find why the commit's CI didn't run
  or didn't fail on the suite (`.github/justfile` `test`, the CI lane that
  runs it, the land gate) and fix that gap rather than adding a path rule.
- **jq noise.** `python3 sh/rs/uring.test.py` passes but prints
  `jq: error (at <stdin>:36): null (null) cannot be matched, as it is not a
  string` four times. Its cargo-metadata fixture has no `workspace_root` or
  `targets`, which the seed filter in `sh/rs/select.sh` (around line 66)
  reads. Make the fixture complete, or make the filter tolerate their
  absence, whichever matches real `cargo metadata` output.

Public API: none. Wire: none.

## Related

- [workspace.test.py ignores the caller's gate filter](/quest/m0/ci/workspace-test-env.md) - #7432's quest, the same dispatcher test environment
- [moq-dev/moq#7440](https://github.com/moq-dev/moq/pull/7440) - plans `quest/m0/ci/check-selection.md`, which makes a jq error in Rust selection fail the run; that turns this jq noise into a failure, so land the fixture fix first or together. It does not cover the tool-list selection gap
