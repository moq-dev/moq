# [S] Release tags push to nix.moq.dev, and Cachix is retired

## Goal

Tags cut from `release` push their Nix packages to nix.moq.dev through
`release-nix.yml`, `release` no longer runs `cachix.yml`, and once a release
tag's uploads and a clean-machine `nix run` substitution are proven, the
`CACHIX_AUTH_TOKEN` repository secret and the `kixelated` Cachix cache are
deleted.

## Plan

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ backport now and retire
Cachix after the next patch cut on `release`, rather than waiting for the m2
cut to merge `main` into `release`. Not chosen: retiring it at the m2 cut with
`CACHIX_AUTH_TOKEN` copied into `publish` meanwhile; retiring it at the m2 cut
with the token left at repository scope. This pulls the Cachix part out of
the release Nix cache quest that
[moq-dev/moq#7662](https://github.com/moq-dev/moq/pull/7662) adds in m2, which keeps
only what needs the cut.

Today `release`'s `cachix.yml` pushes `moq-relay-v*`, `moq-cli-v*`,
`moq-boy-v*`, `libmoq-v*` and `moq-gst-v*` tag builds to Cachix. Its job names
`publish`, which does not hold `CACHIX_AUTH_TOKEN`, so it reads the repository
copy. That copy is what blocks step 3 of
[Keep pull-request CI read-only](/quest/m0/ci/read-only-secrets.md); retiring
Cachix removes it instead of moving it.

- Backport `main`'s `release-nix.yml` to `release` and delete `cachix.yml`
  there, in one PR against `release` that the maintainer merges (agents never
  merge into `release`). Match `release`'s own tag patterns (it still cuts
  `libmoq-v*`, where `main` cuts `moq-c-v*`) and the flake attributes
  `release` builds; the `cache-write` Environment must admit those tags.
- Prove it on the first tag after the backport: Release Nix uploads succeed
  on all three platforms, and a clean machine substitutes the tagged package
  from nix.moq.dev.
- Then delete `CACHIX_AUTH_TOKEN` and the `kixelated` cache (a Cachix login,
  so `Maintainer:`), and drop Cachix from docs that still name it.

## Related

- [Publish off main](/quest/m0/ci/publish-release-only.md) - the other workflow backport to `release` decided in the 2026-11-05 audit
