# [XS] The video corpus has an R2 bucket

## Goal

An R2 bucket holds the video-quality corpus. Open clips are public-read at a
moq.dev hostname, the way ROMs are served from `rom.moq.dev`. Private clips
sit under a prefix that only a read-only token can fetch, and that token is a
secret the nightly video-quality jobs can read.

This waits on a Cloudflare account action. Check it by fetching one public
object anonymously, and one private object with the secret from a nightly
`workflow_dispatch` run. Delete this quest once both work.

## Plan

- Follow the existing R2 secret naming (`MBX_R2_*`, `R2_*`), with a separate
  token that is read-only and scoped to this bucket. The corpus must never
  share write credentials with the caches.
- Upload is a maintainer action. The manifest the harness owns records each
  object's sha256, so a replaced object fails the run instead of silently
  changing the baseline.
