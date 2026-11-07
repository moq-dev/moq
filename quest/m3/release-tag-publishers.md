# [S] Binary release publishers run on their tag

## Goal

`release-brew`, `release-go` and `release-swift-lib` run as their release tag,
not through `workflow_run` on `main`, like the publishers
[moq-dev/moq#7994](https://github.com/moq-dev/moq/pull/7994) moved.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431). They use
the `bot` Environment today, which admits `main`; moving them needs `bot` to
admit the binary release tags, a maintainer action to list in the PR.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ moved from m0 to m2 (m3 since the milestone re-cut) behind
[the cut](/quest/m3/release/cut.md), since tags run `release`'s copies of the
workflows and `release` gets `main`'s only when the cut merges it. Not
chosen: moving all four release-gated m0 CI quests, including
Drop main from the publish Environment
([moq-dev/moq#7994](https://github.com/moq-dev/moq/pull/7994)), which backports its own change instead; merging `main` into `release` now,
which reverses the 2026-11-04 decision.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - brings `main`'s workflows to `release`, where tags run them
