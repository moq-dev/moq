# [S] Release tags push to nix.moq.dev, and Cachix stops taking writes

## Goal

Tags cut from `release` push their Nix packages to nix.moq.dev through
`release-nix.yml`, and a clean machine substitutes a tagged package from
there. `release` no longer runs `cachix.yml`, and `CACHIX_AUTH_TOKEN` is
deleted. Both nix.moq.dev bucket sizes hold under the CI bucket's 14-day
expiry.

## Plan

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ backport now and retire
Cachix after the first `release` tag that proves nix.moq.dev, rather than
waiting for the m2 cut to merge `main` into `release`. Not chosen: retiring it
at the m2 cut with `CACHIX_AUTH_TOKEN` copied into `publish` meanwhile;
retiring it at the m2 cut with the token left at repository scope. This
absorbs [moq-dev/moq#7662](https://github.com/moq-dev/moq/pull/7662)'s m2
"Release tags substitute from nix.moq.dev" quest, which it had split out
behind the cut; the backport removes its only reason to wait.

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ revoke the token but keep
the `kixelated` cache read-only. Older tags' flakes list only
`kixelated.cachix.org`, so deleting the cache would make `nix run` on them
build from source. Not chosen: copying the retained release paths to
nix.moq.dev first (old flakes still only name Cachix); deleting the cache and
accepting source builds.

Today tags run `release`'s copies of the workflows. `release` still has
`cachix.yml` and `cache.yml` and no `release-nix.yml`, so every tag so far,
through `moq-relay-v0.17.12` and `moq-cli-v0.14.12` on 2026-11-03, pushed to
Cachix and never reached nix.moq.dev. `cachix.yml` names `publish`, which does
not hold `CACHIX_AUTH_TOKEN`, so it reads the repository copy. That copy is
what [Keep pull-request CI read-only](/quest/m0/ci/read-only-secrets.md) would
otherwise have to move.

- Backport to `release`, in one PR the maintainer merges (agents never merge
  into `release`): `main`'s `release-nix.yml`, the `infra/nix` Just module and
  its entry in `infra/justfile` (`release` has neither, so `just infra nix
  push` would fail), any local action it uses, the deletion of
  `cachix.yml`, and `main`'s `cache.yml` in place of `release`'s, whose
  dispatch-only `warm` job still runs `cachix push kixelated` with
  `CACHIX_AUTH_TOKEN` and would fail once the token is gone. Match `release`'s own tag patterns (it still cuts `libmoq-v*`
  where `main` cuts `moq-c-v*`) and the flake attributes `release` builds; the
  `cache-write` Environment must admit those tags.
- On the first `moq-relay-v*` or `moq-cli-v*` tag after the backport, record
  its `release-nix.yml` pushes from all three legs, then
  `nix run github:moq-dev/moq/<tag>#moq-relay --accept-flake-config`
  substituting on a clean machine (`#moq` for a CLI tag), such as a fresh
  container with only Nix installed.
- Then the maintainer deletes `CACHIX_AUTH_TOKEN` (a repository secret, so
  `Maintainer:`), and docs stop naming Cachix as a current cache.

Public API: none. Wire: none.

## Waiting

- Maintainer: record both bucket sizes no earlier than 2026-11-24, a month
  after the first `cache.yml` push to `nix-ci-moq-dev` on 2026-10-24, against
  the 14-day expiry in `infra/nix/lifecycle.json`. Sizes need a
  `wrangler login` to the Cloudflare account, which agents lack. A path
  expires 14 days after upload even while CI still reads it, and `nix copy`
  re-uploads it on the next merge. Look for Nightly jobs that fell back to
  source builds once paths expired (`building '/nix/store/...uniffi-bindgen`
  in a job log), and refresh on a schedule or switch to a closure-aware
  collector (niks3) if they recur.
