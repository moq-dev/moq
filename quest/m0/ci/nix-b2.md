# [M] nix.moq.dev serves from Backblaze B2

## Goal

nix.moq.dev's Worker serves the release and CI caches from private B2
buckets in the `us-east-005` account, whose Flamethrower credits run to about
2027-11-17. `main`'s `cache.yml` and `release-nix.yml` on both `main` and
`release` push there. Afterward, a clean machine substitutes a tagged package
and a CI dev shell from nix.moq.dev. R2's `nix-moq-dev` and `nix-ci-moq-dev`
and the `NIX_R2_*` tokens are deleted.

Non-goals: moq's other R2 buckets, and changing the CI cache's 14-day expiry.

## Plan

Decided 2026-11-23 (maintainer, /quest-plan), with the move itself and its
reasons in [the Rust cache move](/quest/m0/ci/mbx-b2.md):

- ✅ Private buckets, with the Worker signing each GET (SigV4) with a
  read-only key held as a Worker secret (rejected: public buckets, which let
  anyone download straight from B2 on billed egress and list the bucket). B2
  egress to Cloudflare is free under the Bandwidth Alliance.
- ✅ Backport the push change to `release` in a PR the maintainer merges, as
  [moq-dev/moq#8062](https://github.com/moq-dev/moq/pull/8062) did, and cut
  the Worker over only after it lands (rejected: the Worker also reading R2
  until the m3 cut merges `main` into `release`). Agents never merge into
  `release`.
- ✅ Copy `nix-moq-dev`'s 1.8 GB of released packages, which old tags'
  flakes still substitute; the CI bucket starts empty (rejected: copying
  `nix-ci-moq-dev`'s 73.5 GB too, which expires within 14 days).
- ✅ Delete the R2 buckets and revoke their tokens in this PR once the
  substitution checks pass (rejected: keeping them a week).
- ✅ An agent creates the buckets and keys in the B2 console through the
  maintainer's agent Chrome, as for mbx.

Recommendations:

- Order: B2 buckets and keys, then `main`'s writers and the `release`
  backport, then the copy from R2, then the Worker deploy (`just infra nix
  deploy`). A tag pushed between the writer switch and the deploy reaches B2
  and is served once the Worker moves.
- Keep the Worker's release-then-CI order and its rule that an unavailable
  read is a miss sent with `no-store`, mapped from R2's error codes to B2's
  5xx answers. Prefer a small, maintained SigV4 signer such as aws4fetch to a
  hand-rolled one.
- `refresh` restarts an object's age by copying it onto itself. On B2 that
  writes a new version, and the lifecycle counts from the newest upload.
  Deleting hidden versions a day later keeps superseded copies from piling up;
  check that a refreshed object's age actually restarts.
- If [cachix-retire](/quest/m0/ci/cachix-retire.md) is still open, point its
  bucket-size check at the B2 buckets.
- Update everything that names R2 for nix: `infra/nix/`, `infra/README.md`,
  `cache.yml`, `release-nix.yml`, and the
  [CI README](/quest/m0/ci/README.md)'s caches bullet.

Validation: in a fresh `docker.io/nixos/nix` container, `nix run
github:moq-dev/moq/<tag>#moq-relay --accept-flake-config -- --version` builds
nothing, both for a tag copied from R2 and one pushed to B2. The same works
for a CI dev shell from `main`, and Worker logs show the reads going to B2.

Public API: none. Wire: none.

## Related

- [The Rust cache moves to B2](/quest/m0/ci/mbx-b2.md) - same account, console session, and R2 teardown
- [Retire Cachix](/quest/m0/ci/cachix-retire.md) - measures these buckets' sizes
