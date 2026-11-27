# [S] GitHub Pages deploys find the artifact their job uploaded

## Goal

Nightly's `Benchmark site` and Smoke's `Publish relay report` (and Interop's report, which
deploys the same way) never fail with `No artifacts named "github-pages" were found` after a
successful `upload-pages-artifact` in the same job, over two weeks of scheduled runs.

## Plan

Failed on 2026-11-20 and 11-22 (Benchmark site), and 11-25 (both: Smoke
[45675725911](https://github.com/moq-dev/moq/actions/runs/45675725911) and Nightly
[45667362304](https://github.com/moq-dev/moq/actions/runs/45667362304)). Each time
`actions/deploy-pages` v5.0.1 logged `Fetching artifact metadata failed` about a second after
the upload finished. `sh/gh/pages-clean.sh` deletes only this run's earlier `github-pages`
artifacts, before the upload, so it isn't deleting the new one.

Decided 2026-11-26 (maintainer, /quest-plan): ✅ plan a quest (rejected: keep watching; drop).

Find why the lookup misses: an eventual-consistency window in the artifacts API, or a
deploy-pages bug fixed upstream. GitHub's documented pattern uploads in a build job and
deploys from a separate `needs:` job, which may close the window. Fix the cause rather than
retry the deploy. Keep in mind that the three workflows share one Pages site.

Public API: none. Wire: none.
