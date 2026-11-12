# [XS] Release tags push to nix.moq.dev, and Cachix stops taking writes

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

Recorded 2026-11-07 (/quest-iterate): the first tags after the backport,
`moq-relay-v0.17.14` and `moq-cli-v0.14.14` from
[moq-dev/moq#8096](https://github.com/moq-dev/moq/pull/8096), pushed from all
three Release Nix legs. Each of `macos-latest`, `ubuntu-24.04-arm`, and
`ubuntu-latest` logged `copying path ... to 's3://nix-moq-dev?...'` for its
package in runs
[42321547211](https://github.com/moq-dev/moq/actions/runs/42321547211) (relay)
and [42321696950](https://github.com/moq-dev/moq/actions/runs/42321696950)
(CLI). In a fresh `nixos/nix` container (Nix 2.35.4, x86_64-linux),
`nix run github:moq-dev/moq/<tag>#<pkg> --accept-flake-config -- --version`
built nothing for either tag: it fetched
`9qidys7z...-moq-relay-0.17.14` and `y2j3pxyl...-moq-cli-0.14.14`, the paths
the `ubuntu-latest` legs pushed, from `https://nix.moq.dev` and their seven
runtime dependencies from `cache.nixos.org`.

`main` names Cachix nowhere outside `quest/`. `release`'s workflows still list
`kixelated.cachix.org` as a read-only substituter; the m2 cut's merge of
`main` replaces them, so they need no backport.

Deleted 2026-11-07 (maintainer, /quest-unblock): the `CACHIX_AUTH_TOKEN`
repository secret. No workflow on `main` or `release` read it, no Environment
held a copy, and `gh secret list --repo moq-dev/moq` no longer lists it. The
`kixelated` cache stays readable for old tags.

Decided 2026-11-07 (maintainer, /quest-unblock): agents collect the bucket
sizes with this desktop's wrangler login.

Baseline 2026-11-07 (`npx wrangler r2 bucket info <bucket>`):
`nix-ci-moq-dev` held 11,019 objects and 29.3 GB, and `nix-moq-dev` held 45
objects and 151 MB.

What remains: record both bucket sizes against the CI bucket's 30-day expiry.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: no earlier than 2026-11-24, a month after the first
  `cache.yml` push to `nix-ci-moq-dev` on 2026-10-24, an agent on the
  maintainer's desktop records both buckets' sizes with
  `npx wrangler r2 bucket info nix-moq-dev` and `nix-ci-moq-dev`, against the
  30-day expiry in `infra/nix/lifecycle.json`. A path expires 30 days after
  upload even while CI still reads it, and `nix copy` re-uploads it on the
  next merge. Look for Nightly jobs that fell back to source builds once paths
  expired (`building '/nix/store/...uniffi-bindgen` in a job log), and refresh
  on a schedule or switch to a closure-aware collector (niks3) if they recur.
