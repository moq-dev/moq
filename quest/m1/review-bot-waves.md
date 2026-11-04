# [XS] Bot reviewers don't re-review every push

## Goal

CodeRabbit and Pullfrog review a PR when it's marked ready (and on request),
not on every push, so fixing review findings stops drawing a fresh wave of
threads that holds the PR out of the merge queue. Done when a PR iterated
through three or more pushes after ready gets a bot review when marked ready,
no new bot threads on the later pushes, and a new bot review when one is
requested, and the PR records that PR as evidence.

## Plan

[moq-dev/moq#7896](https://github.com/moq-dev/moq/pull/7896) added
`.coderabbit.yaml` (no incremental reviews, drafts skipped, UI settings
inherited). Pullfrog's re-review trigger has no repo-side setting; its docs
place it only in the console. Agents never request an online review
(`CONTRIBUTING.md`), so `AGENTS.md` stays unchanged; a maintainer requests one
with `@coderabbitai review` or an `@pullfrog` mention.

Once both waits clear, take that PR, check each bot's reviews and threads against the Goal, request one
review per bot if none was requested, and delete this quest in a PR citing it.

## Waiting

- Maintainer: in the Pullfrog console, Automations → Review PRs, turn off
  "Re-review when new commits are pushed" and leave "Include draft PRs" off.
  No API or repo file sets it. Check: a push to a ready PR draws no new
  `pullfrog[bot]` review.
- moq-dev/moq: a PR whose branch holds `.coderabbit.yaml` (CodeRabbit reads
  it from the PR branch) gets three or more pushes after ready, after the
  Pullfrog toggle above. Check: its timeline's `ready_for_review` event and
  the `committed` events after it.
