# [XS] The video corpus has R2 buckets

## Goal

Two R2 buckets hold the video-quality corpus. Open clips are in a public-read
bucket at a moq.dev hostname, the way ROMs are served from `rom.moq.dev`.
Private clips are in a separate bucket with public access off, since R2's
public access exposes a whole bucket and a prefix cannot be private behind
it. A read-only token for the private bucket is a secret the nightly
video-quality jobs can read.

## Plan

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ a checked-in wrangler
setup creates the buckets. Not chosen: running the wrangler commands once by
hand with nothing checked in; leaving it all to the maintainer.

- Add a `just create` recipe under `infra/`, the way `infra/nix/justfile`
  creates the nix.moq.dev buckets: both buckets, with public access and a
  moq.dev custom domain (such as `corpus.moq.dev`) on the open one only.
  Run it with the desktop's existing `wrangler login`; wrangler there reaches
  the Cloudflare account.
- Follow the existing R2 secret naming (`MBX_R2_*`, `R2_*`), with a separate
  token that is read-only and scoped to the private bucket. The corpus must
  never share write credentials with the caches.
- Upload is a maintainer action. The manifest the harness owns records each
  object's sha256, so a replaced object fails the run instead of silently
  changing the baseline.

## Waiting

- Maintainer: once the setup has created the private bucket, mint its
  read-only token in the dashboard (R2, Manage API tokens, Object Read only,
  scoped to that bucket) and store it with `gh secret set` under the names
  the setup documents. Wrangler cannot create R2 API tokens, and an agent
  never handles the secret. Check by fetching one public object anonymously,
  one private object with the secret from a nightly `workflow_dispatch` run,
  and the same private object anonymously, which must fail. Delete this quest
  once all three hold.
