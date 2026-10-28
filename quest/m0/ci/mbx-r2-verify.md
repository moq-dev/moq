# [S] Verify the R2 Rust cache

## Goal

Confirm the cache earns its keep for the hosted jobs that still use R2
(Nightly on `main`, release pull requests, and the macOS and Windows jobs),
and record the numbers in the PR that closes this:

- Each Nightly job's `Check the cache` step reports `effective read-write`,
  and a release pull request's reports `effective read-only`.
- Nightly's Linux, macOS and Windows jobs hit what the previous Nightly
  wrote.
- After the mbx 1.28.0 bump, the merge queue reports no new
  `error[E0463]: can't find crate`, and the next scheduled Nightly's macOS
  job reports no new oversized-manifest digest.
- Restore plus build time against the 150-210s Actions restore of the ~6 GB
  store it replaced, from the same commit before and after.
- Each object expires 30 days after upload, so a crate no Nightly builds in
  that window stays cold for the next hosted job. Count how often a Nightly
  job pays that rebuild.
- Bucket size and request counts after a month, against the 30-day expiry in
  `infra/mbx/lifecycle.json`. Shorten or lengthen it from that.

Use scheduled Nightly runs on `main` only (`gh run list --workflow
nightly.yml --branch main --event schedule`), never a dispatch on a branch.

## Plan

Decided 2026-10-22 (maintainer): narrowed to hosted jobs. Since
[#6040](https://github.com/moq-dev/moq/pull/6040) the merge queue runs on the
self-hosted `moq-queue` runner with mbx's local store, so it neither reads nor
writes R2.

Evidence so far, from the platform jobs of scheduled Nightlies
[39600361153](https://github.com/moq-dev/moq/actions/runs/39600361153)
(2026-10-21, mbx 1.25.0),
[39779729461](https://github.com/moq-dev/moq/actions/runs/39779729461)
(2026-10-22, 1.25.0) and
[39951870847](https://github.com/moq-dev/moq/actions/runs/39951870847)
(2026-10-23, 1.26.0):

- The bootstrap is done: Windows and macOS report `configured read-write,
  effective read-write`. That proves credentials and policy, not reuse.
- macOS refuses task manifests written past the reader's 16 MiB bound
  (`a90076a4...` at 17204435 bytes, then `5cb8fb79...` and `b0310ce4...`), so
  those tasks get no remote predictions. Two more were refused before the
  bump, both under mbx 1.25.0: `2ed9df47...` (17218002 bytes) on 2026-10-25
  ([40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303))
  and 2026-10-26, and `0d4f4e3f...` (17290876 bytes) on 2026-10-26
  ([40332938121](https://github.com/moq-dev/moq/actions/runs/40332938121)). No macOS summary downloads more than
  15 KiB. Reported as
  [jdx/mr-boxington#792](https://github.com/jdx/mr-boxington/issues/792) and
  fixed by [jdx/mr-boxington#798](https://github.com/jdx/mr-boxington/pull/798),
  which keeps the read bound and caps the writer instead. The Rust cache
  now pins 1.28.0, the first release carrying the fix. It does not repair
  manifests already oversized on R2: if task identities carry over, the
  five known digests may still refuse until the 30-day lifecycle deletes
  them, about 2026-11-22 for the first three and 2026-11-26 for the last
  two. A refusal for any other digest means the fix did not hold.
- Windows logs no refusal but still no reuse. Its first build reports 0 hits
  and 1084 of 1091 compilations "not looked up" for want of a matching
  prediction on 2026-10-22, after a 1.25.0 Nightly wrote the day before, and
  1091 of 1098 on 1.26.0. The manifest fix does not explain this. If it
  persists after the bump, find why before timing builds.
- No release pull request has run with the R2 cache yet.

Decided 2026-10-27 (maintainer): the cross-job miss report moved to
[unifying dependency features](/quest/m0/ci/hakari.md), which measures its own
before and after baseline and no longer waits on this quest.

## Related

- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - the same R2 and Environment bootstrap for Nix
