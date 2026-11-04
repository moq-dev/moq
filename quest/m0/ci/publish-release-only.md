# [XS] Drop main from the publish Environment

## Goal

The `publish` Environment, which holds registry tokens, signing keys, and
deploy credentials, admits only `release` and the protected release tags
(`moq-ffi-v*`, `moq-relay-v*`, `moq-cli-v*`). Code that lands on `main` can no
longer publish a package or sign a release. This waits on the maintainer's
live GitHub settings changes below; check them with
`gh api repos/moq-dev/moq/environments/publish/deployment-branch-policies`
and `gh api repos/moq-dev/moq/environments/publish/secrets`.

## Plan

[moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431) moved every job
that names `publish` off `main`: the tag's own run dispatches `release-winget`
on the tag and calls the Kotlin wrapper, instead of chaining them by
`workflow_run`. apt and rpm dispatches name `--ref release`, and `alert` names
the `alert` Environment, which the maintainer created on 2026-11-02 (admits
only `main`, holds `DISCORD_WEBHOOK`).

The same PR dropped Cachix from `main` (decided 2026-11-02): Smoke's nix
channel substitutes from `nix.moq.dev` and no longer pushes, so it names no
Environment and `CACHIX_AUTH_TOKEN` needs no new home. The alternatives were
moving the token into `cache-write`, or deleting Cachix everywhere now with a
`release` backport. `release` keeps its Cachix workflows until the next cut
merges `main` into it, and
[nix-cache-verify](/quest/m0/ci/nix-cache-verify.md) deletes the Cachix
cache and token once a release tag substitutes from `nix.moq.dev`.

What remains is the maintainer's, in order:

1. Merge `main` into `release` (a release cut or a backport). Tags run
   `release`'s copies of the workflows, so until then a tag still chains its
   winget and Kotlin wrapper publishers through `main`'s `workflow_run`, which
   that PR removed: those two skip, and `release`'s apt and rpm dispatches
   still run on `main`.
2. Then drop `main` from `publish`'s deployment policy, and remove
   `DISCORD_WEBHOOK` and `CACHIX_AUTH_TOKEN` from `publish`.

Decided 2026-11-04 (maintainer, /quest-unblock): ✅ cut the combined patch
release on `release` soon, once [moq-dev/moq#7596](https://github.com/moq-dev/moq/pull/7596)'s
remaining ports and [moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680)
are ready, and merge `main` into `release` for the next minor line
separately, later. Not chosen: one combined cut that merges `main` first, or
neither this week.

Public API: none. Wire: none.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - moves the repository secrets into the Environments this names
