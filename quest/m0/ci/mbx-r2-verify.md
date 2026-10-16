# [S] Verify the R2 Rust cache

## Goal

Condition: a maintainer has run the `mbx-moq-dev` bootstrap in
`infra/README.md` (bucket, lifecycle rule, both R2 tokens, the `cache-write`
Environment and its secrets), and `cache.yml` has warmed every leg from `main`
since. Check it with `gh run list --workflow cache.yml --branch main`.

Then confirm the cache earns its keep, and record the numbers in the PR that
closes this:

- Each warm's `Check the cache` step reports `effective read-write`, and a
  pull request's reports `effective read-only`.
- Check, Test, and the macOS and Windows merge queue jobs show mbx remote hits
  for code the pull request did not touch.
- Restore plus build time against the 150-210s Actions restore of the ~6 GB
  store it replaced, from the same diff before and after.
- Bucket size and request counts after a month, against the 30-day expiry in
  `infra/mbx/lifecycle.json`. Shorten or lengthen it from that.

## Related

- [Require platform gates](/quest/m0/ci/platform-queue.md) - the macOS and Windows jobs this cache serves
- [Nix cache](/quest/m0/ci/nix-cache.md) - the same R2 and Environment bootstrap for Nix
