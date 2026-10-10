# [XS] main merges through a squash queue

## Goal

Condition: the maintainer enables "Require merge queue" with method Squash on
`main` in ruleset 2420853, keeping Check and Test as required checks. No bot
bypass is needed: nothing merges `release` into `main` automatically, and the
maintainer lands the pre-release `release` into `main` merge with their own
admin bypass.

Check: `gh api repos/moq-dev/moq/rulesets/2420853` lists a `merge_queue` rule
with merge method `SQUASH` (it has none as of 2026-10-09). Delete this quest
once it holds and the Dependabot check below is done. Advance it by asking the
maintainer to flip the ruleset.

## Plan

`CONTRIBUTING.md`'s Commits and Merge queue sections already describe the
queue as live. They become true when this clears; if the plan changes
instead, fix them in the same PR.

Check once the queue is on: `.github/workflows/dependabot.yml` merges with the
moq-bot app token through `gh pr merge --auto --squash`. On the next
Dependabot PR, confirm it went through the queue, and record the result in the
PR that deletes this quest.
