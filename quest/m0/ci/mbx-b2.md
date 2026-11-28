# [XS] R2's Rust cache is torn down

## Goal

Once the Rust cache reads and writes Backblaze B2 (`mbx-moq-dev` in the
`us-east-005` account), the second scheduled Nightly on B2 hits at least as
well as the last scheduled Nightly on R2, and then R2's `mbx-moq-dev`
bucket, its `mbx-read` and `mbx-write` API tokens, and the `MBX_R2_*`
secrets (repository and `cache-write` Environment) are deleted.

Non-goals: moq's other R2 buckets (nix.moq.dev moved separately in
[moq-dev/moq#9625](https://github.com/moq-dev/moq/pull/9625)) and longer-term measurement, which
[the B2 verification](/quest/m0/ci/mbx-b2-verify.md) owns.

## Plan

Decided 2026-11-23 (maintainer, /quest-plan): ✅ move mbx and nix.moq.dev to
B2 for its startup credits, start the B2 bucket empty, expire objects 30
days after upload, and delete the R2 bucket and its tokens once a Nightly
hits B2 (rejected: keeping them a week as a fallback). The move itself
landed in [moq-dev/moq#9624](https://github.com/moq-dev/moq/pull/9624). Only
`main` writes the cache, so no Nightly could hit B2 before that PR merged;
the teardown it was to include is this quest.

Decided 2026-11-25 (maintainer, /quest-run): ✅ the R2 teardown stays this
separate [XS] quest (rejected: folding it into
[the B2 verification](/quest/m0/ci/mbx-b2-verify.md)), and ✅ the B2 bucket
keeps the name `mbx-moq-dev` (rejected: renaming it).

Steps:

1. Compare each Rust-cache job's hits and "not looked up" counts
   (`mbx[cache]: object cache` lines) between the last scheduled Nightly on
   R2 and the second on B2. Without conditional writes, concurrent writers in
   one namespace can overwrite each other's task manifests, which shows as
   one job's "not looked up" rising while totals hold. If B2 hits clearly
   less, or a job's "not looked up" rises, stop and bring it to the
   maintainer before deleting anything.

   Found 2026-11-27: the stop condition fired, so nothing is deleted yet.
   Totals across every job's `object cache` lines, scheduled Nightlies on
   `main`:

   | Nightly | Remote | Hits | Misses | Not looked up | Remote failures |
   | ------- | ------ | ---- | ------ | ------------- | --------------- |
   | [11-24](https://github.com/moq-dev/moq/actions/runs/45459585582) | R2 | 20,932 | 6,310 | 12,199 | 71 |
   | [11-25](https://github.com/moq-dev/moq/actions/runs/45667362304) | R2 | 16,677 | 2,107 | 22,795 | 55 |
   | [11-26](https://github.com/moq-dev/moq/actions/runs/45867457463) | B2 | 17,028 | 1,485 | 17,877 | 469 |
   | [11-27](https://github.com/moq-dev/moq/actions/runs/46038678705) | B2 | 16,077 | 1,961 | 20,918 | 166 |

   The second B2 Nightly hit 3.6% less than the last R2 one, inside R2's own
   night-to-night swing, but eight jobs went from warm to fully "not looked
   up" (`rs hls-conformance` 14 to 466, `test resume-model` 6 to 367, `rs
   cli-export`, `rs doctest --workspace`, `rs rung-disable`, Media (firefox
   player), `features`, moxygen). Each logged "a manifest predicting N
   compilations was loaded, but none matched this build; the compiler or its
   flags changed", against one to three such jobs per R2 Nightly. No toolchain
   moved between the two B2 Nightlies (`Cargo.lock` changed one line and
   `.config/hakari.toml` excluded kio), and the other jobs on the same
   compiler stayed warm, so a manifest one job overwrote with another
   compiler's or flags' predictions fits better than a toolchain change.
   Confirm it by finding which build wrote the manifests those jobs loaded:
   the B2 bucket's keys are `b2-us-east-key-id` and `b2-us-east-key` in
   Bitwarden Secrets Manager. R2 has taken no writes since 2026-11-25, so its
   30-day expiry empties it by about 2026-12-25 and holding it costs little.
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

- Maintainer: On the second Nightly after the Rust cache moved from R2 to
  B2, total hits held (16,077 against R2's 16,677) but eight jobs went fully
  cold after loading a task manifest that matched none of their
  compilations, against one to three per R2 Nightly. B2 refuses the
  conditional writes R2 used to keep concurrent jobs from overwriting each
  other's manifests, and that overwrite fits the evidence. Deleting R2's
  bucket, tokens, and `MBX_R2_*` secrets is irreversible. Tear R2 down now?
  - Keep R2 until an agent confirms or rules out manifest overwrites on B2 (Recommended) - R2 costs little while its objects expire, and keeping it leaves conditional writes one revert away if overwrites are the cause
  - Tear R2 down now - totals sit inside R2's own night-to-night swing (20,932 then 16,677 hits), so accept the per-job churn
  - Move the Rust cache back to R2 - keeps conditional writes and drops the B2 credits

## Related

- [moq.pro's B2 account move](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/b2-account.md) - the `us-east-005` account and its credits
