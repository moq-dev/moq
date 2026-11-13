# [M] Re-measure planned breaks in the release pipeline

## Goal

Each release that publishes a package a planned break pins (moq-cli,
moq-relay, `@moq/net`) re-measures those pins against the published versions
and lands the rewritten `test/interop/compat/planned-breaks.json` on `main`
with the release. No scheduled Interop run goes red on a stale pin, and no one
runs the remeasure by hand.

## Plan

Decided 2026-11-11 (maintainer), after moq-cli 0.14.15 and 0.14.16 each made
the `moq-lite-07-wip` pin stale and the scheduled Interop runs on Nov 9 to 11
went red on it:

- ✅ the release pipeline runs `just test wire-compat --remeasure` and lands the rewritten `test/interop/compat/planned-breaks.json` with the release (rejected: keep the manual post-publish step; warn on stale pins in scheduled runs).

Decided 2026-11-12 (maintainer), since stale pins caused four of the last six red scheduled Interop runs. This quest is a child of the [Interop epic](/quest/m0/interop-green/README.md):

- ✅ the release-remeasure pipeline is a child quest of quest/m0/interop-green/ (rejected: quest/m3/release/; drop the follow-up).
- ✅ the release cut keeps its manual post-publish `just test wire-compat --remeasure` step until this quest lands, and does not require it (rejected: block the cut on this quest; accept a red Interop run per release).

The job runs after the publish jobs for the three packages the planned-breaks
file pins, in the workflows that publish from `release` (`.github/workflows/release-*.yml`).
It re-measures the pins against the versions just published and lands the
rewritten file on `main`, which RELEASE.md's release procedure does by
hand today. Check `main`'s ruleset first: where it requires a pull request, the
job opens one. Its write credentials go in an environment limited to protected
branches.

Verify it by running the job on its own against the current pins (moq-cli
0.14.16, moq-relay 0.17.16, `@moq/net` 0.4.7). It should reproduce the file
with no diff.

The same PR updates RELEASE.md's release bullet, which says to run the
remeasure by hand, to name the job. The nightly wire-compat check keeps
running as it does now.
