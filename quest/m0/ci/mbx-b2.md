# [XS] R2's Rust cache is torn down

## Goal

Once the Rust cache reads and writes Backblaze B2 (`mbx-moq-dev` in the
`us-east-005` account), the second scheduled Nightly on B2 hits at least as
well as the last scheduled Nightly on R2, and then R2's `mbx-moq-dev`
bucket, its `mbx-read` and `mbx-write` API tokens, and the `MBX_R2_*`
secrets (repository and `cache-write` Environment) are deleted.

Non-goals: moq's other R2 buckets ([nix.moq.dev](/quest/m0/ci/nix-b2.md)
moves separately) and longer-term measurement, which
[the B2 verification](/quest/m0/ci/mbx-b2-verify.md) owns.

## Plan

Decided 2026-11-23 (maintainer, /quest-plan): ✅ move mbx and nix.moq.dev to
B2 for its startup credits, start the B2 bucket empty, expire objects 30
days after upload, and delete the R2 bucket and its tokens once a Nightly
hits B2 (rejected: keeping them a week as a fallback). The move itself
landed in [moq-dev/moq#9624](https://github.com/moq-dev/moq/pull/9624). Only
`main` writes the cache, so no Nightly could hit B2 before that PR merged;
the teardown it was to include is this quest.

Steps:

1. Compare each Rust-cache job's hits and "not looked up" counts
   (`mbx[cache]: object cache` lines) between the last scheduled Nightly on
   R2 and the second on B2. Without conditional writes, concurrent writers in
   one namespace can overwrite each other's task manifests, which shows as
   one job's "not looked up" rising while totals hold. If B2 hits clearly
   less, or a job's "not looked up" rises, stop and bring it to the
   maintainer before deleting anything.
2. Delete the `MBX_R2_*` secrets: `gh secret delete MBX_R2_ACCESS_KEY_ID`
   and `MBX_R2_SECRET_ACCESS_KEY`, each with and without `--env
   cache-write`, on `moq-dev/moq`. A branch that has not merged `main` since
   the move then reads no key and builds cold with a warning, instead of
   failing on a deleted bucket.
3. Revoke the `mbx-read` and `mbx-write` R2 API tokens in the Cloudflare
   dashboard (R2 -> Manage API tokens), through the maintainer's agent
   Chrome.
4. Empty and delete the bucket. R2 deletes only an empty bucket, so set a
   1-day expiry rule with `npx wrangler r2 bucket lifecycle add mbx-moq-dev`,
   wait a day for it to clear, then `npx wrangler r2 bucket delete
   mbx-moq-dev` (account `dd618f5dbd5da77b8296f1613c301f5c`, under this
   desktop's `wrangler login`).

## Waiting

- moq-dev/moq: the second scheduled Nightly on `main` after
  [moq-dev/moq#9624](https://github.com/moq-dev/moq/pull/9624) merges
  completes its Rust jobs; check with `gh run list --workflow nightly.yml
  --branch main --event schedule`.

## Related

- [nix.moq.dev moves to B2](/quest/m0/ci/nix-b2.md) - same account and R2 teardown
- [moq.pro's B2 account move](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/b2-account.md) - the `us-east-005` account and its credits
