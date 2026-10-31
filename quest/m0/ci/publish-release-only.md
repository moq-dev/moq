# [XS] Drop main from the publish Environment

## Goal

The `publish` Environment, which holds registry tokens, signing keys, and
deploy credentials, admits only `release` and the protected release tags
(`moq-ffi-v*`, `moq-relay-v*`, `moq-cli-v*`). Code that lands on `main` can no
longer publish a package or sign a release. This waits on the maintainer's
live GitHub settings changes below; check them with
`gh api repos/moq-dev/moq/environments/publish/deployment-branch-policies`
and `gh api repos/moq-dev/moq/environments/alert`.

## Plan

[moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431) moved every job
that names `publish` off `main`: `release-winget` and the Kotlin wrapper are
called from their tag's own run instead of chained by `workflow_run`, apt and
rpm dispatches name `--ref release`, `alert` names a new `alert` Environment,
and Smoke's Cachix push names `cache-write`.

What remains is the maintainer's, in order:

1. Before that PR merges, create the `alert` Environment with a deployment
   policy admitting only `main`, holding `DISCORD_WEBHOOK`, and copy
   `CACHIX_AUTH_TOKEN` into `cache-write`. Without the webhook every alert
   fails; without the token Smoke's Cachix push is skipped.
2. Merge `main` into `release` (a release cut or a backport). Tags run
   `release`'s copies of the workflows, so until then a tag still chains its
   winget and Kotlin wrapper publishers through `main`'s `workflow_run`, which
   this PR removed: those two skip, and `release`'s apt and rpm dispatches
   still run on `main`.
3. Then drop `main` from `publish`'s deployment policy, and remove
   `DISCORD_WEBHOOK` and `CACHIX_AUTH_TOKEN` from `publish`.

Public API: none. Wire: none.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - moves the repository secrets into the Environments this names
