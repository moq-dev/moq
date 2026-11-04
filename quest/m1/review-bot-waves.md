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
inherited). Pullfrog keeps its review triggers in its own repository settings,
not in a file: the console's Automations → Review PRs card, or the
[Pullfrog CLI](https://docs.pullfrog.com/cli-configuration) keys
`review.re-review` and `review.drafts`, which need repo write.

- ✅ 2026-11-04: Pullfrog re-review on push turned off and draft reviews left
  off, with the maintainer's authorization:
  `pullfrog config set review.re-review false --repo moq-dev/moq` and
  `pullfrog config set review.drafts false --repo moq-dev/moq`.
  `pullfrog config get` now prints `false` for both (`review.re-review` was
  `true`).

Agents never
request or wait for an online review (`CONTRIBUTING.md`), so `AGENTS.md` stays
unchanged; a maintainer requests one with `@coderabbitai review` or an
`@pullfrog` mention.

Once the wait below clears, take the evidence PR, check each bot's reviews and
threads against the Goal, and delete this quest in a PR that cites it.

## Waiting

- moq-dev/moq: a PR whose branch holds `.coderabbit.yaml` (CodeRabbit reads
  it from the PR branch) gets three or more pushes after ready, after the
  Pullfrog change of 2026-11-04, and then a maintainer requests one review from each
  bot on it and each bot answers. Check: its timeline's `ready_for_review`
  event, the `committed` events after it, the maintainer's request comments,
  and a bot review after each request.
