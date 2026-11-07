# [XS] The video corpus has R2 buckets

## Goal

Two R2 buckets hold the video-quality corpus. Open clips are in a public-read
bucket at a moq.dev hostname, the way ROMs are served from `rom.moq.dev`.
Private clips are in a separate bucket with public access off, since R2's
public access exposes a whole bucket and a prefix cannot be private behind
it. A read-only token for the private bucket is a secret the nightly
video-quality jobs can read.

## Plan

Done 2026-11-07: `just infra corpus create` created `corpus-moq-dev`, public
at `corpus.moq.dev`, and `corpus-private-moq-dev`, with no custom domain and
its r2.dev URL off. Each holds a `probe.txt`; `infra/README.md` ("Bootstrapping
the video corpus") documents the token, the `corpus` Environment, and the
`CORPUS_R2_*` secret names.

- Upload is a maintainer action. The manifest the harness owns records each
  object's sha256, so a replaced object fails the run instead of silently
  changing the baseline.

## Waiting

- Maintainer: mint the private bucket's read-only token in the dashboard and
  store it in the `corpus` Environment, steps 2 to 4 of `infra/README.md`'s
  "Bootstrapping the video corpus". Wrangler cannot create R2 API tokens, and
  an agent never handles the secret. Check by fetching one public object
  anonymously, one private object with the secret from a nightly
  `workflow_dispatch` run, and the same private object anonymously, which must
  fail. Delete this quest once all three hold.
