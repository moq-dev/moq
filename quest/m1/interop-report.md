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
  another writer's pages. Every job in that group sets `queue: max`: the
  default queue keeps one pending job and cancels the older one, which would
  silently skip a night's report when three writers line up. Add it to
  Nightly's `benchmark-site` job too. If [moq-dev/moq#6046](https://github.com/moq-dev/moq/pull/6046)
  has landed, reuse its per-directory publisher (`test/smoke/report/publish.sh`);
  otherwise write the same small one so both can share it. Update the
  single-writer comment on Nightly's `benchmark-site` job to describe the
  per-directory ownership.
- The harness emits structured results next to its logs, one record per
  case: suite, case, publisher, subscriber, transport, version, outcome, and
  duration.
  Suite and case keep apart the media QA controls that share one browser
  pairing (frozen video, silent audio, offset audio, leaked session,
  lagging latecomer). The report is generated from those records, never by
  scraping logs. It checks them against the expected suite and case
  inventory, so a case with no record (the harness died in shared setup or
  mid-suite) shows as a red "not run" cell instead of disappearing.
- The page's `<title>` labels it on the Pages root, which lists every report
  directory ([moq-dev/moq#6079](https://github.com/moq-dev/moq/pull/6079)).
- Only scheduled runs on `main` publish; manual and release-PR runs keep
  their results in the run. The workflow's concurrency group
  (`interop-${{ github.ref }}`, `cancel-in-progress: true`) lets a manual run
  on `main` cancel the scheduled one, losing that night's report: key the
  group by event too, so only a run of the same kind cancels another.
- Re-running a job in place leaves two `github-pages` artifacts, which
  `deploy-pages` refuses. Delete the stale one before a re-run.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m1/audit-ci.md) - judges every workflow's visibility; this closes Interop's gap
