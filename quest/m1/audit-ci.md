# [M] CI workflow audit

## Goal

Every CI workflow and job earns its place: its results reach someone (a gate,
a Discord alert, or a published report) at a cost proportional to that signal.
Each one that doesn't is fixed, moved, or deleted. Individual tests stay with
the [test cost audit](/quest/m1/audit-test-cost.md), and runner and cache
infrastructure with [CI cost and latency](/quest/m0/ci/README.md).

## Plan

Recurs once per major release (decided 2026-10-20). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2), never m3, the deferred bucket
that does not shift with releases. The successor keeps the Goal and this
paragraph; the rest of its Plan is rewritten with what this run learned: the
per-workflow table, baselines, and what to check first. A successor placed in m2 lists the
[release cut](/quest/m2/release/cut.md) under `Required`, so it waits for the
release in flight to ship. The parent milestone lists this quest under
`Required`. Only `/quest-delete` ends the recurrence. Fixes land in the run's
PR, one commit each, rather than as follow-ups. Only a change that needs a
maintainer decision (dropping a release target, scope) or is independently
landable and too large to review alongside the rest becomes a follow-up
through `/quest-plan`.

Split from the test cost audit on 2026-10-20: this audit owns workflows and
jobs, that one owns tests. The first run waits for the one-time
[CI pipeline audit](/quest/m0/ci/audit.md), so it measures the one-pass
pipeline instead of the one it replaces.

Each run tables every workflow in `.github/workflows/` and every Nightly job
with three verdicts:

- **Signal:** what regression it catches that nothing cheaper catches, and
  when it last caught one.
- **Visibility:** who sees a failure or a result. A gate blocks merging, a
  scheduled or publishing workflow posts to Discord `#alerts` through
  [alert.yml](/.github/workflows/alert.yml) (`just gh check` enforces its
  coverage), and a report lands on the
  [Pages site](https://git.moq.dev/) or in a job summary. A result
  only in a run artifact or log is invisible: publish it, summarize it, or
  stop producing it. Each report on the Pages site owns one `gh-pages`
  directory: its workflow commits only that directory, deploys the whole
  branch, and joins the `benchmark-pages` concurrency group with
  `queue: max`, so no deploy drops another report (decided 2026-10-20 with the
  [interop report](/quest/m1/interop-report.md); Nightly's `benchmark-site`
  comment still claims a single writer until the first second writer lands).
- **Cost:** runner-minutes per month by runner type, from the org billing API
  and job timings, against that signal. macOS is the scarce one (a 5-job
  concurrency cap).

Check first (from the test cost audit's m0 run, 2026-10-19):

- Classify every failed job of the latest Nightly, scheduled Interop, and
  Platform runs, not just the slowest. On 2026-10-19, 28 Nightly jobs had
  failed for eight independent causes, some for days. A red Nightly hides the
  next regression: the send-depth gate and all six fuzz targets broke on
  2026-10-18 and stayed broken unnoticed until that audit. Nightly was still
  red on 2026-10-20 (fuzz, media players, audit, features, release builds), so
  an alert that always fires is not visibility either.
- Platform (macOS, Windows) runs only in the merge queue and is not required,
  so a Windows-only compile error in moq-video stayed red for two days across
  dozens of queue runs until
  [#5966](https://github.com/moq-dev/moq/pull/5966). Most Platform failures
  in the sample were that error, a choco `nasm` install outage, or another
  queued PR's breakage. Rank Platform by failure cause, not count.
- A queue failure often belongs to another PR in the same batch: a stale
  fixture in the entry itself, or a semantic conflict with a PR merged after it
  was queued. Check the entry's head branch and what merged around it before
  calling it a flake.
- Owned elsewhere: [nightly media players](/quest/m1/media-engines-nightly.md),
  release builds by [release builds link](/quest/m0/release-link.md), and Rust
  advisories by [the dependency audit](/quest/m1/audit-deps.md).
- Re-running a single job in place cannot redeploy Pages: the earlier
  attempt's `github-pages` artifact remains and `deploy-pages` refuses two.
  Delete the stale artifact first, or wait for the next Nightly.

## Required

- [CI pipeline audit](/quest/m0/ci/audit.md) - the one-time audit of the one-pass pipeline this run starts from
