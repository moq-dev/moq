# [S] Verify the Nix cache

## Goal

Condition: a maintainer has run the `nix.moq.dev` bootstrap in
`infra/README.md` (the release tag ruleset, both buckets and the Worker, the
`cache-write` Environment's tag policies and secrets, and the
signing key), and the PR that wired it in has merged with the real public key
in `flake.nix`. Check it with `curl https://nix.moq.dev/nix-cache-info` and
`gh run list --workflow cache.yml --branch main`.

Then confirm it, and record the numbers in the PR that closes this:

- The first `main` warm pushes the dev shell, and a later pull request job
  enters `nix develop` without compiling uniffi-bindgen-go.
- The next `moq-relay-v*` or `moq-cli-v*` tag pushes from all three
  `release-nix.yml` legs, and `nix run github:moq-dev/moq/<tag>#moq-relay
  --accept-flake-config` substitutes on a clean machine.
- Dev shell upload time and both bucket sizes after a month, against the
  30-day expiry in `infra/nix/lifecycle.json`. Switch to a closure-aware
  collector (niks3) only if expiry makes CI fall back to source builds often.
- Delete the `CACHIX_AUTH_TOKEN` secret and the `kixelated` Cachix cache once
  the maintainer decides downstream flakes have had long enough to switch.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the R2 and Environment decisions this verifies
