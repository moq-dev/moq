# [XS] Pullfrog reviews finish inside their time limit

## Goal

A Pullfrog review of a large PR finishes instead of failing on the
action's run limit: `.github/workflows/pullfrog.yml` sets a `timeout`
and `effort` for the `pullfrog/pullfrog` step that fit the reviews it is
dispatched for, and the choice is recorded in the PR with the run
durations that support it.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run). Milestone: ✅ m0 (user: "use recommended for any CI/flakes"). The fix is to raise the timeout or lower the effort.

Facts from the last 100 runs of the workflow (58 runs, all
`workflow_dispatch` reviews; 55 passed):

- [41178266611](https://github.com/moq-dev/moq/actions/runs/41178266611)
  ("Review #7202"): `agent run timed out after 1h`. The log shows
  `effort: high` and `timeout: 1h (default)`.
- [41188250827](https://github.com/moq-dev/moq/actions/runs/41188250827)
  ("Review #7234", 56 min): aborted by the action's activity watchdog after
  the model sent nothing for 903 s. Not the run limit, but a lower effort
  likely shortens those silent turns too.
- [41197083466](https://github.com/moq-dev/moq/actions/runs/41197083466)
  ("Review #7211", 23 min): failed installing dependencies
  (`@moq/libav@0.1.0 is not published to npm yet, and building it needs
  Nix`). Unrelated to the limit; note it in the PR as a follow-up rather
  than fixing it here.

The action takes `timeout` ("e.g. 10m, 1h30m. Default: 1h") and `effort`
(`low` to `max`, or a position in [0, 1]) inputs; today neither is set in
`with:` (`pullfrog.yml:31-32`), so the repo's Pullfrog settings give
`high`, likely from the repo's Pullfrog settings. The file's header allows
edits only where indicated; `with:` is the place. Recommended: set
`effort: medium` first, since it bounds cost as well as time, and raise
`timeout` only if medium-effort reviews of the largest recent
PRs still approach an hour. Keep the job's own `timeout-minutes` (default
360) above whatever `timeout` is set.

[moq-dev/moq#7317](https://github.com/moq-dev/moq/pull/7317) (pins the
action) and [moq-dev/moq#7424](https://github.com/moq-dev/moq/pull/7424)
(registers Pullfrog in alerts, gates its secrets to main) also edit this
file; whichever lands later merges the others.

Public API: none. Wire: none.
