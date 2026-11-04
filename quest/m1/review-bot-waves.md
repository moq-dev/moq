# [S] Bot reviewers don't re-review every push

## Goal

CodeRabbit and Pullfrog review a PR when it's marked ready (and on request),
not on every push, so fixing review findings stops drawing a fresh wave of
threads that holds the PR out of the merge queue. Done when a PR iterated
through three or more pushes after ready gets no new bot threads unless one is
requested, and the PR records the configuration and one such PR as evidence.

## Plan

Planned 2026-11-04 with the maintainer. On 2026-11-03/04 each push to a ready
PR drew new bot threads, and agents only queue a PR with zero open threads, so
iterations looped: [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652)
collected 24 threads over six pushes, and
[moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752) and
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) each lost a queue
slot to a thread that landed seconds after a push. Several were real bugs, so
the bots stay; the cadence changes.

- **CodeRabbit:** the repo has no `.coderabbit.yaml`, so it runs the app
  defaults (incremental reviews on every push). Add one that turns off
  automatic incremental reviews and skips drafts, keeping the full review on
  ready and `@coderabbitai review` on request.
- **Pullfrog:** its reviews are triggered by the app, not
  `.github/workflows/pullfrog.yml` (dispatch-only). Find whether its trigger is
  repo-configurable; if it's only in the Pullfrog console, the PR names the
  exact setting for the maintainer to change.
- Keep the queue rule "no open threads" as is. Record in `AGENTS.md`, if
  anywhere, how an agent requests a re-review after its last push.
