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
`pull_request`. Delete this quest once both hold.

## Plan

Decided in #4619 (2026-10-05): a squash queue with a pull_request-only bot
bypass for the back-merge, landed by the `land` job; the back-merge may land
against a `main` that moved during its CI run, accepted.

Known risk: the Dependabot workflow mints the same moq-bot app token, so a
Dependabot merge could also skip the queue once moq-bot is a bypass actor.

## Related

- [moq-bot may push workflow changes](/quest/m1/bot-workflows-permission.md) - the other moq-bot permission the back-merge needs
