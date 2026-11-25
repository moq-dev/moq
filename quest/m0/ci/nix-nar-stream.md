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
itself kept failing, which [the B2 move](/quest/m0/ci/nix-b2.md) changes, and
no hop in between remains to fix. That move replaces `bucket.get` with a
signed `fetch`, so it carries the resume over as a `Range` request guarded by
`If-Match` on the first response's etag.

Decided 2026-11-25 (maintainer, /quest-run): ✅ an agent deploys `main`'s
Worker now with this desktop's wrangler login (rejected: waiting for the B2
move's Worker deploy). Deployed 2026-11-25 from `7cab21e65b` as version
`4834140a-e633-4520-b718-27a2e7cc26bc`; an earlier deploy at 17:01Z already
followed #9604's merge, so count runs from 2026-11-26.

## Waiting

- moq-dev/moq: a week of scheduled Nightly runs on `main` from 2026-11-26
  (`gh run list --workflow nightly.yml --branch main --event schedule`). Grep
  each job's log for `disabling binary cache 'https://nix.moq.dev'`; done when
  seven runs show none. `wrangler tail` shows only Worker logs and cannot
  confirm this. If the B2 cutover deploys mid-week, say which runs it covers.
