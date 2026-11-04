# [XS] A release merge opens no stale release PR

## Goal

Merging a release-plz release PR into `release` publishes that release and
opens no new release PR for the versions it just shipped. A new release PR
appears only when `release` holds unreleased changes. Done when a release
merge doesn't produce a stale release PR.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`).

[moq-dev/moq#7651](https://github.com/moq-dev/moq/pull/7651) merged at
19:33Z, and release-plz opened
[moq-dev/moq#7750](https://github.com/moq-dev/moq/pull/7750) at 19:57Z,
before 0.19.33 was published. The only change in #7750 was a second `## [0.19.33]` heading in
`rs/moq-tokio/CHANGELOG.md`, so it was closed. Release PRs routinely open
within half an hour of the previous merge
([moq-dev/moq#7009](https://github.com/moq-dev/moq/pull/7009), opened after
[moq-dev/moq#6508](https://github.com/moq-dev/moq/pull/6508) merged, was
also closed unmerged).

The likely cause: `release-rs.yml` runs `just rs release` on every push to
`release`, and that recipe (`rs/justfile`, `release:`) runs
`release-plz release-pr` before `release-plz release`. The PR step then sees
the merged versions as unpublished and untagged, and re-proposes them.

- Confirm the cause from the workflow run that #7651's merge started, or
  from a dry run (`release-plz release-pr --dry-run` or similar) against the
  merged state.
- If confirmed, run `release-plz release` first, then `release-pr`, in the
  same recipe. Check how release-plz's own action orders the two commands, and
  copy that order.
- Keep the change compatible with
  [Rust releases publish through OIDC](/quest/m1/release-rs-oidc.md), which
  edits the same workflow.

Public API: none. Wire: none.
