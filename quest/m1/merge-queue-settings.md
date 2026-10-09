# [XS] main merges through a squash queue

## Goal

Condition: once `release` carries #4619's `sh/gh/back-merge.sh`, the
maintainer switches `main` to the merge queue in ruleset 2420853, making both
changes at once:

- Enable "Require merge queue" with method Squash, keeping Check and Test as
  required checks.
- Add moq-bot as a bypass actor with mode `pull_request`, so the `land` job in
  `.github/workflows/check.yml` can merge the release back-merge as a merge
  commit outside the queue.

Order matters. Until `release` carries the new script, a push to `release`
runs the old copy, which enables auto-merge on the back-merge PR; under a
squash queue that squashes it. With the queue on but no bypass, `land`'s merge
is refused.

Check: `git show origin/release:sh/gh/back-merge.sh` no longer runs
`gh pr merge --auto`, and
`gh api repos/moq-dev/moq/rulesets/2420853` lists a `merge_queue` rule with
merge method `SQUASH` and a moq-bot bypass actor with `bypass_mode`
`pull_request`. Delete this quest once both hold and the Dependabot check
below is done.

Advance it by backporting #4619's `sh/gh/back-merge.sh` to `release` (a
backport PR, per `CONTRIBUTING.md`); as of 2026-10-08 `release` still runs
`gh pr merge --auto`, and the ruleset has no `merge_queue` rule. Then ask the
maintainer to flip the ruleset.

## Plan

Decided in #4619 (2026-10-05): a squash queue with a pull_request-only bot
bypass for the back-merge, landed by the `land` job; the back-merge may land
against a `main` that moved during its CI run, accepted.

`CONTRIBUTING.md`'s Merge queue section and the `land` job's comment in
`check.yml` already describe the queue as live. They become true when this
clears; if the plan changes instead, fix them in the same PR.

Check once both settings are on: `.github/workflows/dependabot.yml` merges
with the same moq-bot app token through `gh pr merge --auto --squash`, without
`--admin`, so its merges may still enqueue rather than bypass. On the next
Dependabot PR, see whether it went through the queue or skipped it as a
bypass actor, and record the result in the PR that deletes this quest. If it
skipped the queue, ask the maintainer whether that is acceptable.

## Related

- [moq-bot may push workflow changes](/quest/m1/bot-workflows-permission.md) - the other moq-bot permission the back-merge needs
