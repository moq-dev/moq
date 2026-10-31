# [XS] workspace.test.py ignores the caller's gate filter

## Goal

`sh/js/workspace.test.py` passes under any `just gate` module filter, because it no longer inherits `CHECK_MODULES` and the other gate and CI variables from the shell that runs it.

## Plan

Found by the agent on [moq-dev/moq#7356](https://github.com/moq-dev/moq/pull/7356). `test_dispatcher_passes_only_js_impact_paths` builds its environment as `dict(os.environ, ...)` (`sh/js/workspace.test.py:65`) and runs `sh/dispatch.sh ci` (`:74`). Under a filter that drops the `js` module, such as `just gate py`, the stub `just` never writes `calls` and `:75` fails.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Pop the same keys `sh/dispatch-query.test.py:33-37` already pops (`CHECK_MODULES`, `CHECK_JOBS`, `IN_MOQ_DEV_SHELL`, `GITHUB_ACTIONS`, `GITHUB_STEP_SUMMARY`, `GITHUB_BASE_REF`), ideally through one shared helper both tests use.

Public API: none. Wire: none.
