# [XS] Confirm nix.moq.dev no longer drops NAR streams

## Goal

Nightly logs show no `disabling binary cache 'https://nix.moq.dev'` line over a
week of runs after the Worker's body resume (moq-dev/moq#9604) is deployed.

## Plan

The Worker now resumes an R2 body that fails mid-read with a ranged `get`, so
a broken stream no longer reaches Nix as a reset connection. Once deployed,
grep the week's Nightly job logs the way
[moq-dev/moq#8048](https://github.com/moq-dev/moq/pull/8048) did (14 jobs in
5 of 11 runs before). If lines remain, read the Worker's logs for `body read
failed` and `resume failed` at those timestamps: a resume cap hit means R2
itself kept failing, which the B2 move
([moq-dev/moq#9514](https://github.com/moq-dev/moq/pull/9514)) changes, and
no hop in between remains to fix. That move replaces `bucket.get` with a
signed `fetch`, so it carries the resume over as a `Range` request guarded by
`If-Match` on the first response's etag.

## Waiting

- Maintainer: run `just infra nix deploy` (the Worker deploys by hand with
  Cloudflare credentials); check a Nightly run for no cache-disabled line.
  `wrangler tail` shows only Worker logs and cannot confirm this.
