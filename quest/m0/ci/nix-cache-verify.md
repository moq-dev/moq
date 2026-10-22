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

Decided 2026-10-22 (maintainer):
[#6040](https://github.com/moq-dev/moq/pull/6040) deleted `cache.yml`, so
nothing pushes the dev shell to `nix.moq.dev`; only `release-nix.yml` pushes,
on release tags. First measure one hosted Nightly job's dev-shell entry time,
no earlier than 2026-11-20: `cache.yml`'s last push to `nix.moq.dev` ran
2026-10-20, and those paths serve a warm entry until the 30-day expiry
removes them. If it still compiles `uniffi-bindgen-go`, `uniffi-bindgen-cpp` and `quest`
(the 12-14 minute baseline below), restore a dev-shell push. Otherwise this
narrows to release-tag substitution and Cachix retirement.

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
