# [XS] The Python wrapper publish can be retried by hand

## Goal

`.github/workflows/release-py.yml` has a `workflow_dispatch` trigger, so a
maintainer can retry a failed `moq-rs` publish from `release` without a new
`moq-ffi-v*` tag or a path-matching push. A dispatch from any ref other than
`release` never publishes.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#6634](https://github.com/moq-dev/moq/pull/6634).

Milestone: ✅ m1 (moved from m2). Not chosen: m2. There is no m1 release
epic, so this sits at the m1 top level.

- Today the workflow runs only on a `release` push touching `py/moq-rs/**`,
  the workflow, or `sh/gh/release.sh`, or on `workflow_run` after a
  `moq-ffi-v*` tag (`release-py.yml:15`). When the `moq-ffi-v0.4.20` chain
  failed, the only retries were a new tag or a matching push.
- Follow `release-kt-lib.yml`, which allows `workflow_dispatch` and publishes
  only when `github.ref == 'refs/heads/release'` (`release-kt-lib.yml:193`).
  Keep `release-py.yml`'s existing gates (already on PyPI, moq-ffi published).
- A dispatch with `--ref release` runs `release`'s copy of the workflow, so
  the trigger helps only once it is on `release` too. Backporting it is a
  `release` push touching the workflow, which itself publishes; coordinate
  with the maintainer's cut instead of pushing it.
- Keep it consistent with
  [Release credentials off main](/quest/m0/ci/publish-release-only.md): a
  dispatch adds no new path to the `pypi` environment from `main`.

Public API: none. Wire: none.

## Related

- [Publish and adopt the breaking release](/quest/m2/release/README.md) - release publishing
