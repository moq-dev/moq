# [S] Verify the R2 Rust cache

## Goal

Condition: the `mbx-moq-dev` bootstrap in `infra/README.md` is complete and
scheduled Nightly runs on `main` have built Rust since. Check it with
`gh run list --workflow nightly.yml --branch main`.

Then confirm the cache earns its keep for the hosted jobs that still use R2
(Nightly on `main`, release pull requests, and the macOS and Windows jobs),
and record the numbers in the PR that closes this:

- Each Nightly job's `Check the cache` step reports `effective read-write`,
  and a release pull request's reports `effective read-only`.
- Nightly's Linux, macOS and Windows jobs hit what the previous Nightly
  wrote.
- Restore plus build time against the 150-210s Actions restore of the ~6 GB
  store it replaced, from the same commit before and after.
- Each object expires 30 days after upload, so a crate no Nightly builds in
  that window stays cold for the next hosted job. Count how often a Nightly
  job pays that rebuild.
- Bucket size and request counts after a month, against the 30-day expiry in
  `infra/mbx/lifecycle.json`. Shorten or lengthen it from that.

## Plan

Decided 2026-10-22 (maintainer): narrowed to hosted jobs. Since
[#6040](https://github.com/moq-dev/moq/pull/6040) the merge queue runs on the
self-hosted `moq-queue` runner with mbx's local store, so it neither reads nor
writes R2.

## Related

- [Require the release gate](/quest/m0/ci/release-gates.md) - the macOS and Windows jobs this cache serves
- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - the same R2 and Environment bootstrap for Nix
