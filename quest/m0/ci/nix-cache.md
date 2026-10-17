# [M] nix.moq.dev replaces Cachix

## Goal

`https://nix.moq.dev` is the project's Nix binary cache, served from R2. CI
jobs substitute the dev shells from it, release workflows push the released
packages to it, and `flake.nix` and every workflow name it instead of
`kixelated.cachix.org`. Only workflows on `main` or a release tag can sign
and push.

## Plan

- Cachix's open-source plan is 5 GB and evicts the oldest paths, which the
  dev shell closures plus three-platform release builds outgrow. R2 has no
  such cap and no egress charge.
- Nix verifies signatures, so the signing key, not the bucket key, is the
  trust boundary. Keep it and the bucket key in GitHub Environments that
  `main` and the release tags can deploy from, and nothing else. Release tags
  are unprotected today (no tag ruleset), so restricting who can create them
  comes first; a separate release Environment from the mbx one is fine. A
  leaked bucket key cannot make a client accept a path.
- Serve the bucket like apt and rpm: a Worker in `infra/nix` with a checked-in
  `wrangler.jsonc` and custom domain, cache headers suited to `.narinfo`
  (short) and `nar/` (immutable). Bucket and lifecycle are one-time commands
  in `infra/README.md`.
- Expiry by object age can delete a dependency before its dependent. Clients
  should set `fallback = true` and the warmer re-pushes what it needs; switch
  to a closure-aware collector (niks3) only if that proves noisy.
- Downstream flakes that trust `kixelated.cachix.org` for released packages
  keep working until they update; decide with the maintainer how long Cachix
  keeps receiving release pushes.
- The cache warmer's dev shell push and the Cachix release workflow both move
  over.
- Measured 2026-10-16 (Nightly run 38897397428): every Nightly workload
  checked spent 12-14 minutes building `uniffi-bindgen-go`,
  `uniffi-bindgen-cpp` and `quest` before entering the dev shell. Cachix had
  no copy; the warmer's run for that commit had been cancelled.

## Related

- [mbx on R2](/quest/m0/ci/mbx-r2.md) - shares the Environment and bootstrap pattern, not the bucket
