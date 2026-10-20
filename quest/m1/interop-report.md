# [S] Interop report

## Goal

The nightly Interop run publishes its results to
<https://git.moq.dev/interop/>: a color-coded pass/fail table of
publisher by subscriber language for each transport and protocol version,
plus the media QA and other suites. A red cell links to its run and failure
artifact. Today the results exist only in job logs.

## Plan

Decided with the maintainer on 2026-10-20.

- `interop.yml` writes the report. It is another `gh-pages` writer that
  commits only its own `interop/` directory, deploys the whole branch, and
  joins Nightly's `benchmark-pages` concurrency group so deploys never drop
  another writer's pages. If [moq-dev/moq#6046](https://github.com/moq-dev/moq/pull/6046)
  has landed, reuse its per-directory publisher (`test/smoke/report/publish.sh`);
  otherwise write the same small one so both can share it. Update the
  single-writer comment on Nightly's `benchmark-site` job to describe the
  per-directory ownership.
- The harness emits structured results (one record per cell: client pair,
  transport, version, outcome, duration) next to its logs, and the report is
  generated from those, never by scraping logs.
- The page's `<title>` labels it on the Pages root, which lists every report
  directory ([moq-dev/moq#6079](https://github.com/moq-dev/moq/pull/6079)).
- Only scheduled runs on `main` publish; manual and release-PR runs keep
  their results in the run.
- Re-running a job in place leaves two `github-pages` artifacts, which
  `deploy-pages` refuses. Delete the stale one before a re-run.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m1/audit-ci.md) - judges every workflow's visibility; this closes Interop's gap
