# [S] Verify the Nix cache

## Goal

`nix.moq.dev` serves CI and releases: hosted Nix jobs enter the dev shell
without compiling it, release tags substitute on a clean machine, its bucket
sizes hold under the 30-day expiry, and Cachix is retired.

## Plan

[#5693](https://github.com/moq-dev/moq/pull/5693) wired `nix.moq.dev` into
CI. The maintainer ran the bootstrap in `infra/README.md` on 2026-10-19 and 2026-10-20 (the
release tag ruleset, both buckets and the Worker, the `cache-write`
Environment's tag policies and secrets, and the signing key). Check it with
`curl https://nix.moq.dev/nix-cache-info`.

[#6588](https://github.com/moq-dev/moq/pull/6588) restored `cache.yml`
after [#6040](https://github.com/moq-dev/moq/pull/6040) deleted it: every
merge to `main` pushes the dev shell's build closure for x86 Linux, ARM Linux,
and ARM macOS to `nix-ci-moq-dev` (`sh/gh/cache.sh`), and `release-nix.yml`
pushes on release tags. So the dev-shell entry time can be measured on any
scheduled Nightly now, and the expiry check below applies.

Then confirm it, and record the numbers in the PR that closes this:

- A hosted Nightly job enters `nix develop` without compiling
  uniffi-bindgen-go. The baseline, measured 2026-10-16 in Nightly run
  38897397428: every workload spent 12-14 minutes building
  `uniffi-bindgen-go`, `uniffi-bindgen-cpp` and `quest` before entering the
  dev shell, because Cachix had no copy.
- The next `moq-relay-v*` or `moq-cli-v*` tag pushes from all three
  `release-nix.yml` legs, and `nix run github:moq-dev/moq/<tag>#moq-relay
  --accept-flake-config` substitutes on a clean machine.
- Both bucket sizes after a month, against the 30-day expiry in
  `infra/nix/lifecycle.json`. A path expires 30 days after its upload even
  while CI still reads it. If a dev-shell push is restored, look for jobs
  that fell back to source builds once its paths expired, and refresh it on a
  schedule or switch to a closure-aware collector (niks3) if they recur.
- Delete the `CACHIX_AUTH_TOKEN` secret and the `kixelated` Cachix cache once
  the maintainer decides downstream flakes have had long enough to switch,
  along with `smoke.yml`'s Cachix substituter and its push step for the nix
  channel.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the R2 and Environment decisions this verifies
