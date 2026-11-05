# [S] Binary release publishers run on their tag

## Goal

`release-brew`, `release-go` and `release-swift-lib` run as their release tag,
not through `workflow_run` on `main`, like the publishers
[Release credentials off main](/quest/m0/ci/publish-release-only.md) moves.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431). They use
the `bot` Environment today, which admits `main`; moving them needs `bot` to
admit the binary release tags, a maintainer action to list in the PR.

## Required

- [Release credentials off main](/quest/m0/ci/publish-release-only.md) - the same move for the `publish` jobs lands first
