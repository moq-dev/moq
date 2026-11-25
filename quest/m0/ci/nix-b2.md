# [S] nix.moq.dev serves from Backblaze B2

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
reasons in [the Rust cache move](/quest/m0/ci/mbx-b2.md): private buckets
read by the Worker with SigV4; the `release` backport merged by the
maintainer before the Worker cuts over; only `nix-moq-dev`'s released
packages copied; buckets and keys made in the B2 console; R2 deleted once
the substitution checks pass.

Done in [moq-dev/moq#9625](https://github.com/moq-dev/moq/pull/9625): the
Worker, writers, lifecycle, and docs; the B2 buckets with their lifecycle
rules; one key per bucket and role (the console scopes a key to one bucket or
all), stored as the Worker's `B2_{RELEASE,CI}_*` secrets and `cache-write`'s
`NIX_B2_{RELEASE,CI}_*`; and R2 `nix-moq-dev`'s 1,628 cache objects copied
to B2, checked by size and MD5. The `release` backport is
[moq-dev/moq#9628](https://github.com/moq-dev/moq/pull/9628).

What remains, in order:

1. Once #9628 merges and `gh run list --workflow release-nix.yml --status
   queued` and `--status in_progress` (each with `--limit 1000`) list
   nothing, revoke the R2 token behind `NIX_R2_*` (Cloudflare dashboard, R2
   -> Manage API tokens) so a pre-backport tag fails loudly.
2. List R2 `nix-moq-dev` through the Cloudflare API (wrangler's OAuth token
   can) and copy to B2 whatever it lacks; `rclone copy --files-from <keys>
   --no-traverse --http-url https://nix.moq.dev :http: b2:nix-moq-dev`
   reads them through the Worker while it still serves R2. The write needs a
   short-lived Read and Write key for `nix-moq-dev` from the console, deleted
   afterward.
3. Once #9625 lands, wait until `just gh cache-rev`, the commit Smoke pins
   (the newest with a successful push-triggered Cache run), is #9625's merge
   or later, so B2 holds its closure; then `just infra nix deploy` (the
   Worker reads B2 from then on). Until then the pinned closure is only in
   R2, which the old Worker still serves.
4. In a fresh `docker.io/nixos/nix` container, `nix run
   github:moq-dev/moq/<tag>#moq-relay --accept-flake-config -- --version`
   builds nothing for a copied tag (`moq-relay-v0.17.18` already did, through
   the B2 Worker in `wrangler dev`), and for the first tag pushed to B2 once
   one exists; the same for a CI dev shell from `main`. Worker logs show no
   read errors.
5. Delete R2 `nix-moq-dev` and `nix-ci-moq-dev`, and the `NIX_R2_*` secrets
   in `cache-write`.

Record which Nightly runs the B2 deploy covers in
[nix-nar-stream](/quest/m0/ci/nix-nar-stream.md).

Public API: none. Wire: none.

## Waiting

- Maintainer: merge [moq-dev/moq#9628](https://github.com/moq-dev/moq/pull/9628)
  into `release` (agents never merge there); its `NIX_B2_RELEASE_*` secrets
  already exist. Check with `gh pr view 9628 --json state`.

## Related

- [The Rust cache moves to B2](/quest/m0/ci/mbx-b2.md) - same account, console session, and R2 teardown
- [One broken NAR download](/quest/m0/ci/nix-nar-stream.md) - fixes the Worker's NAR streams broken mid-download, on B2 once this lands
