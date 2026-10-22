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

## Plan

Verification checkpoint, 2026-10-21 (Pacific):

- `cache-write` admits `gh-readonly-queue/main/*`. Current queue jobs use the
  self-hosted runner's local cache; hosted R2 measurements below come from
  scheduled Nightly [39600361153](https://github.com/moq-dev/moq/actions/runs/39600361153).
- [Windows](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223136733)
  and [macOS](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223136736)
  both report a compatible R2 namespace and `configured read-write, effective
  read-write`. This proves credentials and policy, not cache reuse.
- macOS repeatedly refuses task manifest
  `a90076a403e4add7587d176ba0f95ec1b79b991c730c07b9b9602c4662a627f4`:
  `remote cache action manifest declared 17204435 bytes, over the 16777216
  byte limit`. All eleven object-cache summaries report `0 B downloaded`;
  they include local hits and remote failures. The platform job succeeds,
  but this run does not establish remote reuse or a timing improvement.
- The pinned mbx 1.25.0 bounds remote JSON at 16 MiB in
  [`mbx-cache-core/src/lib.rs`](https://github.com/jdx/mr-boxington/blob/v1.25.0/crates/mbx-cache-core/src/lib.rs#L88).
  Its [prediction pruning](https://github.com/jdx/mr-boxington/blob/v1.25.0/crates/mbx-cache-core/src/agent/manifest.rs#L81)
  bounds entry count, so a manifest can still exceed the serialized-byte
  limit. Fix producer sizing upstream before treating these results as a
  cache pass; preserve the reader's bound rather than raising it to fit one
  observed payload. Confirm the saved manifest before attributing every
  remote miss to this refusal.
- No open upstream issue or PR identified this failure on 2026-10-21.
  [mbx 1.26.0](https://github.com/jdx/mr-boxington/releases/tag/v1.26.0)
  does not list a manifest fix. Posting to `jdx/mr-boxington` needs maintainer
  approval under this repository's external-post policy; no report was sent.

Remaining verification: remote hits across consecutive hosted Nightlies,
release PR read-only policy, a comparable before/after build measurement,
and bucket/request/expiry measurements after a full month. Keep this quest
until those observations and the manifest failure are resolved.

## Related

- [Require the release gate](/quest/m0/ci/release-gates.md) - the macOS and Windows jobs this cache serves
- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - the same R2 and Environment bootstrap for Nix
