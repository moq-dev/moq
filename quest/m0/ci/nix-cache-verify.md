# [S] Verify the Nix cache

## Goal

`nix.moq.dev` serves releases: release tags substitute on a clean machine,
its bucket sizes hold under the 30-day expiry, and Cachix is retired.

## Plan

[#5693](https://github.com/moq-dev/moq/pull/5693) wired `nix.moq.dev` into
CI. The maintainer ran the bootstrap in `infra/README.md` on 2026-10-19 and 2026-10-20 (the
release tag ruleset, both buckets and the Worker, the `cache-write`
Environment's tag policies and secrets, and the signing key). Check it with
`curl https://nix.moq.dev/nix-cache-info`.

[#6588](https://github.com/moq-dev/moq/pull/6588) restored `cache.yml`:
every merge to `main` pushes the dev shell's build closure for x86 Linux, ARM
Linux, and ARM macOS to `nix-ci-moq-dev` (`sh/gh/cache.sh`), and
`release-nix.yml` pushes on release tags. Hosted CI entering the dev shell is
verified: Nightly run 40506938673 (2026-10-27) substituted it in about a
minute with nothing compiled, against 12-14 minutes of source builds in the
2026-10-16 baseline.

`cache.yml` now also builds and pushes the `moq-relay` and `moq` runtime
closures to `nix-ci-moq-dev` on all three platforms (decided 2026-10-29).
Smoke's nix channel reads them without write credentials. Verify the first
post-merge Cache run uploads both and the next Smoke nix run substitutes
them; a local build does not prove hosted publication.

[moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431) dropped Cachix
from `main` (decided 2026-11-02): Smoke substitutes from `nix.moq.dev` and
no longer pushes. `curl https://nix.moq.dev/nix-cache-info` still succeeds
on 2026-11-02. `release-nix.yml` still has no runs that day.

## Waiting

- moq-dev/moq: After this cache coverage lands, record the first successful
  Cache uploads of both runtime closures and a Smoke nix run substituting
  them from `nix.moq.dev`.
- Maintainer: Cut a release from `main`, then record the first `moq-relay-v*`
  or `moq-cli-v*` tag's `release-nix.yml` pushes from all three legs, and
  `nix run github:moq-dev/moq/<tag>#moq-relay --accept-flake-config`
  substituting on a clean machine (use `#moq` for a CLI tag).
  Tags are cut from `release`, which still has `cachix.yml` and `cache.yml`
  and no `release-nix.yml`, so every tag so far, through `moq-relay-v0.17.9`
  and `moq-cli-v0.14.9` on 2026-10-28, ran Cachix push and never reached
  `nix.moq.dev`. Check with `gh run list --workflow release-nix.yml`; it stays
  empty until a release cut merges `main` into `release`.
- Maintainer: Record both bucket sizes after a month, against the CI bucket's
  30-day expiry in `infra/nix/lifecycle.json`, so no earlier than 2026-11-24: the first
  `cache.yml` push to `nix-ci-moq-dev` ran 2026-10-24. Sizes need a
  `wrangler login` to the Cloudflare account. A path expires 30 days after
  its upload even while CI still reads it, and `nix copy` re-uploads it on the
  next merge. Look for Nightly jobs that fell back to source builds once
  paths expired (`building '/nix/store/...uniffi-bindgen` in a job log), and
  refresh on a schedule or switch to a closure-aware collector (niks3) if
  they recur.
- Maintainer: Retire Cachix once the first release cut from `main` shows
  `release-nix.yml` succeeding on all three legs and the clean-machine
  `nix run` above substitutes (decided 2026-10-29): that is the first release
  a downstream user could fetch from `nix.moq.dev`. Then delete the
  `CACHIX_AUTH_TOKEN` secret and the `kixelated` Cachix cache.
  `release` keeps `cachix.yml` until that cut merges `main` into it.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the R2 and Environment decisions this verifies
