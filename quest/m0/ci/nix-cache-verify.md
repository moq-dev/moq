# [S] Verify the Nix cache

## Goal

Condition: the PR that wired `nix.moq.dev` into CI has merged. The maintainer
ran the bootstrap in `infra/README.md` on 2026-10-19 and 2026-10-20 (the
release tag ruleset, both buckets and the Worker, the `cache-write`
Environment's tag policies and secrets, and the signing key). Check it with
`curl https://nix.moq.dev/nix-cache-info` and
`gh run list --workflow cache.yml --branch main`.

Then confirm it, and record the numbers in the PR that closes this:

- The first `main` warm pushes the dev shell, and a later merge queue or
  Nightly job enters `nix develop` without compiling uniffi-bindgen-go. The
  baseline, measured 2026-10-16 in Nightly run 38897397428: every workload
  spent 12-14 minutes building `uniffi-bindgen-go`, `uniffi-bindgen-cpp` and
  `quest` before entering the dev shell, because Cachix had no copy.
- The next `moq-relay-v*` or `moq-cli-v*` tag pushes from all three
  `release-nix.yml` legs, and `nix run github:moq-dev/moq/<tag>#moq-relay
  --accept-flake-config` substitutes on a clean machine.
- Dev shell upload time and both bucket sizes after a month, against the
  30-day expiry in `infra/nix/lifecycle.json`. A path expires 30 days after
  its upload even while CI still reads it, and only the next warm restores
  it, which runs on a flake or `nix/**` change or a dispatch. Look for jobs
  that fell back to source builds in that window. If they recur, add a daily
  schedule to `cache.yml` (and alert on its scheduled failures) or switch to
  a closure-aware collector (niks3).
- Delete the `CACHIX_AUTH_TOKEN` secret and the `kixelated` Cachix cache once
  the maintainer decides downstream flakes have had long enough to switch.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the R2 and Environment decisions this verifies
