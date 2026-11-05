# [S] Release tags substitute from nix.moq.dev

## Goal

The first release tag cut from `main` substitutes from `nix.moq.dev` on a
clean machine, both bucket sizes hold under the CI bucket's 30-day expiry, and
Cachix is retired.

## Plan

Decided 2026-11-05 (maintainer, /quest-audit,
[moq-dev/moq#7975](https://github.com/moq-dev/moq/pull/7975)): ✅ split from
[Verify the Nix runtime cache](/quest/m0/ci/nix-cache-verify.md) and moved to
m2 behind [the cut](/quest/m2/release/cut.md), which brings `main`'s
`release-nix.yml` to `release` when it merges `main` into it. Not chosen:
moving the whole verification quest; merging `main` into `release` now,
which reverses the 2026-11-04 decision.

Tags run `release`'s copies of the workflows. `release` still has
`cachix.yml` and `cache.yml` and no `release-nix.yml`, so every tag so far,
through `moq-relay-v0.17.9` and `moq-cli-v0.14.9` on 2026-10-28, pushed to
Cachix and never reached `nix.moq.dev`;
`gh run list --workflow release-nix.yml` stays empty until the cut.

Once the cut lands:

1. Record the first `moq-relay-v*` or `moq-cli-v*` tag's `release-nix.yml`
   pushes from all three legs.
2. Record `nix run github:moq-dev/moq/<tag>#moq-relay --accept-flake-config`
   substituting on a clean machine (`#moq` for a CLI tag), such as a fresh
   container with only Nix installed.
3. Retire Cachix (decided 2026-10-29): that tag is the first release a
   downstream user could fetch from `nix.moq.dev`. The cut also brings
   `main`'s removal of `cachix.yml` to `release`.

Public API: none. Wire: none.

## Waiting

- Maintainer: Record both bucket sizes no earlier than 2026-11-24, a month
  after the first `cache.yml` push to `nix-ci-moq-dev` on 2026-10-24, against
  the 30-day expiry in `infra/nix/lifecycle.json`. Sizes need a
  `wrangler login` to the Cloudflare account, which agents lack. A path
  expires 30 days after upload even while CI still reads it, and `nix copy`
  re-uploads it on the next merge. Look for Nightly jobs that fell back to
  source builds once paths expired (`building '/nix/store/...uniffi-bindgen`
  in a job log), and refresh on a schedule or switch to a closure-aware
  collector (niks3) if they recur.
- Maintainer: After step 2, delete the `CACHIX_AUTH_TOKEN` secret and the
  `kixelated` Cachix cache; the Cachix account is the maintainer's, and
  deleting the cache is irreversible.

## Required

- [Cut and publish the prepared breaking release](/quest/m2/release/cut.md) - brings `release-nix.yml` to `release`, where tags run it

## Related

- [Verify the Nix runtime cache](/quest/m0/ci/nix-cache-verify.md) - the same cache's `main` evidence
