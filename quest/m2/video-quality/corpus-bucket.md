# [XS] The video corpus has R2 buckets

## Goal

Two R2 buckets hold the video-quality corpus. Open clips are in a public-read
bucket at a moq.dev hostname, the way ROMs are served from `rom.moq.dev`.
Private clips are in a separate bucket with public access off, since R2's
public access exposes a whole bucket and a prefix cannot be private behind
it. A read-only token for the private bucket is a secret the nightly
video-quality jobs can read.

This waits on a Cloudflare account action. Check it by fetching one public
object anonymously, one private object with the secret from a nightly
`workflow_dispatch` run, and the same private object anonymously, which must
fail. Delete this quest once all three hold.

## Plan

- Follow the existing R2 secret naming (`MBX_R2_*`, `R2_*`), with a separate
  token that is read-only and scoped to the private bucket. The corpus must
  never share write credentials with the caches.
- Upload is a maintainer action. The manifest the harness owns records each
  object's sha256, so a replaced object fails the run instead of silently
  changing the baseline.
