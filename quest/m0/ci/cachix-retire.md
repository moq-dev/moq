# [S] Release tags push to nix.moq.dev, and Cachix stops taking writes

## Goal

Tags cut from `release` push their Nix packages to nix.moq.dev through
`release-nix.yml`, and a clean machine substitutes a tagged package from
there. `release` no longer runs `cachix.yml`, and `CACHIX_AUTH_TOKEN` is
deleted. Both nix.moq.dev bucket sizes hold under the CI bucket's 30-day
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

[moq-dev/moq#8062](https://github.com/moq-dev/moq/pull/8062) backported this
to `release` on 2026-11-06: `main`'s `release-nix.yml` (on `release`'s
`libmoq-v*` tag and `libmoq` attribute), the `infra/nix` Just module, `main`'s
`cache.yml`, the deletion of `cachix.yml`, and a `flake.nix` `nixConfig` that
names nix.moq.dev instead of Cachix. Every tag before it, through
`moq-relay-v0.17.13` on 2026-11-05, pushed only to Cachix.

- On the first `moq-relay-v*` or `moq-cli-v*` tag after the backport, record
  its `release-nix.yml` pushes from all three legs, then
  `nix run github:moq-dev/moq/<tag>#moq-relay --accept-flake-config`
  substituting on a clean machine (`#moq` for a CLI tag), such as a fresh
  container with only Nix installed. An agent can run both checks once the
  tag exists.
- Then the maintainer deletes `CACHIX_AUTH_TOKEN` (a repository secret, so
  `Maintainer:`), and docs stop naming Cachix as a current cache.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: the first `moq-relay-v*` or `moq-cli-v*` tag cut from
  `release` after 2026-11-06. Check:
  `gh run list --workflow release-nix.yml --json headBranch,conclusion`.
- Maintainer: record both bucket sizes no earlier than 2026-11-24, a month
  after the first `cache.yml` push to `nix-ci-moq-dev` on 2026-10-24, against
  the 30-day expiry in `infra/nix/lifecycle.json`. Sizes need a
  `wrangler login` to the Cloudflare account, which agents lack. A path
  expires 30 days after upload even while CI still reads it, and `nix copy`
  re-uploads it on the next merge. Look for Nightly jobs that fell back to
  source builds once paths expired (`building '/nix/store/...uniffi-bindgen`
  in a job log), and refresh on a schedule or switch to a closure-aware
  collector (niks3) if they recur.

## Related

- [Verify the Nix runtime cache](/quest/m0/ci/nix-cache-verify.md) - the same cache's `main` evidence
- [Drop main from the publish Environment](/quest/m0/ci/publish-release-only.md) - the other workflow backport to `release` decided in the 2026-11-05 audit
