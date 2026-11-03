# [XS] Pullfrog reviews finish inside their time limit

## Goal

A Pullfrog review of a large PR finishes inside the action's run limit,
with its effective effort and duration recorded as evidence.

## Plan

Decided 2026-10-31 (user, /quest-plan): use the recommended effort-first
budget. Reconfirmed 2026-11-03 (user, /quest-spawn implementation brief):
apply that configuration, publish the attempt, keep unavailable medium-effort
runtime proof as explicit Waiting, and do not raise timeouts without the
required evidence or dispatch online validation.

The workflow now requests `effort: medium` and explicitly retains
`timeout: 1h` in the action's allowed `with:` block. Do not raise the limit
without medium-effort evidence from the largest recent PRs. GitHub's
unchanged default job limit is 360 minutes.

Refreshed 2026-11-03 against pinned action
[`895164fc90f5c493e72693c919c6cf269af158eb`](https://github.com/pullfrog/pullfrog/blob/895164fc90f5c493e72693c919c6cf269af158eb/action.yml).
Its `effort` input overrides repository settings, but JSON prompt fields
have higher precedence in
[`utils/payload.ts`](https://github.com/pullfrog/pullfrog/blob/895164fc90f5c493e72693c919c6cf269af158eb/utils/payload.ts#L337).
More importantly,
[`main.ts`](https://github.com/pullfrog/pullfrog/blob/895164fc90f5c493e72693c919c6cf269af158eb/main.ts#L496)
unconditionally replaces effort for OSS proxy runs with the funded model's
`high` rung. The current
[run 41614007828](https://github.com/moq-dev/moq/actions/runs/41614007828)
uses `proxy: oss` and logs `effort: high`. Setting the input alone therefore
does not establish an effective medium-effort review on this route.

Existing run evidence, measured from the jobs API's `Run agent` step:

- [41178266611](https://github.com/moq-dev/moq/actions/runs/41178266611):
  60m29s, 16 files, high effort, `agent run timed out after 1h`.
- [41362378749](https://github.com/moq-dev/moq/actions/runs/41362378749):
  60m29s, 14 files, high effort, the same run-limit failure.
- [41404718134](https://github.com/moq-dev/moq/actions/runs/41404718134):
  60m40s, 3 files, high effort, the same run-limit failure. Size alone
  does not explain the overruns.
- [41188250827](https://github.com/moq-dev/moq/actions/runs/41188250827):
  56m00s, activity-watchdog failure after 903 seconds without events,
  distinct from the run limit.
- [41590494064](https://github.com/moq-dev/moq/actions/runs/41590494064):
  25m54s, 12 files, high effort, success. This is not medium-effort proof.

The old
[41197083466](https://github.com/moq-dev/moq/actions/runs/41197083466)
logged the dependency-install error but ultimately failed the activity
watchdog at 23m07s. The install error still appears in successful
[41591680387](https://github.com/moq-dev/moq/actions/runs/41591680387) and
[41614007828](https://github.com/moq-dev/moq/actions/runs/41614007828):
`@moq/libav@0.1.0` is unpublished and Nix is absent. The npm version endpoint
still returns 404 on 2026-11-03. Publication is already covered by
[Claim the new npm names](/quest/m2/release/npm-first-publish.md); report the
Pullfrog setup gap to that work's planner rather than duplicating it here.

Do not dispatch or request online reviews to validate this attempt. Use
existing run evidence; leave unavailable runtime proof explicit.

Public API: none. Wire: none.

## Waiting

- Maintainer: choose how to make the effort input effective on the current
  OSS proxy route, where the pinned action forces high effort. Recommended:
  request upstream support for the input; alternative: choose a different
  provider or funding route. No upstream request has been filed by this
  attempt. Keep this PR draft pending that decision, then confirm the chosen
  path in action source and effective-effort logs without dispatching a
  validation run. Do not increase the timeout without the required evidence.
- moq-dev/moq: an existing review of a large recent PR must demonstrate the
  effective effort requested by this workflow and finish inside one hour.
  Record its URL and `Run agent` duration before closing this quest. No such
  medium-effort runtime evidence was available for this attempt.
