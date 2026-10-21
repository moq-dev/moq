# [S] Verify the R2 Rust cache

## Goal

Condition: the `mbx-moq-dev` bootstrap in `infra/README.md` is complete,
including the `cache-write` Environment's `gh-readonly-queue/main/*` branch
policy, and the merge queue has merged Rust changes since. Check it with
`gh run list --workflow check.yml --event merge_group`.

Then confirm the cache earns its keep, and record the numbers in the PR that
closes this:

- Each merge queue and `main` job's `Check the cache` step reports
  `effective read-write`, and a release pull request's reports
  `effective read-only`.
- The merge queue's Check show mbx remote hits for code the queued
  pull request did not touch, and Nightly's macOS and Windows jobs hit what
  the previous Nightly wrote.
- Restore plus build time against the 150-210s Actions restore of the ~6 GB
  store it replaced, from the same diff before and after.
- Each object expires 30 days after upload, and the queue builds only what
  its diff selects, so an expired crate stays cold until a queue or `main`
  job builds it again. Count how often a queue entry pays that rebuild.
- Bucket size and request counts after a month, against the 30-day expiry in
  `infra/mbx/lifecycle.json`. Shorten or lengthen it from that.

## Related

- [Require the release gates](/quest/m0/ci/release-gates.md) - the macOS and Windows jobs this cache serves
- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - the same R2 and Environment bootstrap for Nix
