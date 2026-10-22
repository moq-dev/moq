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

Verification checkpoint, 2026-10-21 (Pacific):

- Hosted R2 measurements below come from scheduled Nightly
  [39600361153](https://github.com/moq-dev/moq/actions/runs/39600361153).
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
