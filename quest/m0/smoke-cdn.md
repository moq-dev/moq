# [S] Diagnose jsDelivr failures in published browser smoke

## Goal

Smoke's jsDelivr lane explains a failed module load with enough evidence to
distinguish CDN delivery from a package or media regression, and any
reproducible first-party defect is fixed at its source.

## Plan

In the [2026-10-15 Smoke run](https://github.com/moq-dev/smoke/actions/runs/38739639896),
apt and Docker each fail five jsDelivr subscriber cells. Early navigations
time out; later browser logs report a missing CORS header on
`https://cdn.jsdelivr.net/npm/@moq/watch/element/+esm`. The same CDN works
for later cells and the cargo job. A subsequent request returned HTTP 200
with `Access-Control-Allow-Origin: *`.

The failed responses' status and body were not retained, so a transient CDN
failure is a hypothesis, not a demonstrated root cause. Work belongs in
moq-dev/smoke, whose client installs and browser navigation own the evidence.
Capture the failing request/response and page error context in CI artifacts,
then reproduce before changing package loading or publication.

Preserve the real CDN lane, latest-package policy, and red media cells.
Do not add exchange retries, longer timeouts, or a local bundle fallback.
Wire any diagnostic regression into smoke's CI. No product documentation
change is needed for test diagnostics alone.

The maintainer requested fixes or quests for the investigated failures on
2026-10-15; the exact CDN cause remains open.
