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

Decided 2026-11-28 (maintainer, /quest-run): ✅ keep R2 until an agent
confirms or rules out manifest overwrites on B2 (rejected: tear R2 down now;
move the Rust cache back to R2).

Decided 2026-11-28 (maintainer, /quest-run pick round): ✅ tear R2 down
after the first Nightly with the shallow-writer fix shows no job loading a
manifest that matches none of its build (rejected: only once mbx stops
losing manifest updates; move the Rust cache back to R2), and ✅ raise mbx's
lost manifest updates upstream as
[jdx/mr-boxington#1030](https://github.com/jdx/mr-boxington/issues/1030)
(rejected: accept the loss). That issue does not gate the teardown.

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
   Found 2026-11-28, from the B2 bucket's object versions (B2 keeps an
   overwritten version for a day): two separate effects.

   - The eight cold jobs were not B2's doing. The Cargo.lock change made a new
     task manifest key, and mbx seeds a new key from earlier lockfile states
     in Git history only while no manifest exists under it. `x11 / SHM and
     GetImage` (Linux-ARM64) published the key's first manifest at 17:17:32
     ("19 of 534 predicted actions were not published": 534 - 19 = 515) from
     a depth-1 checkout, so unseeded. Every Linux-ARM64 job that started
     after it loaded those 515 x11 predictions instead of history and matched
     none; the ones that started later hit, because earlier cold jobs had
     merged their own predictions in by then. R2's create-only write lets the
     first writer win the same way. Fixed in this quest's PR: every cache
     writer checks out full history (`fetch-depth: 0`, `filter: blob:none`,
     about 5 s more per job), and `.github/actions/rust-cache` refuses a
     shallow writer.
   - Overwrites on B2 are real. mbx merges against the manifest it loaded
     when the build started and then writes unconditionally, so a writer
     drops whatever others published while it built (`doctest` at 17:26:19
     kept 779 of the 1,018 predictions `Media (firefox player)` published 26
     s earlier). That night, 76 versions of the Linux-ARM64 manifest
     published 4,680 distinct predictions between them, but the final version
     kept 2,327. That costs prefetch coverage, not correctness, and only mbx
     can fix it: it has no setting to give manifests their own key, and a
     per-job namespace would split the content-addressed results jobs share.

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

- moq-dev/moq: the first scheduled Nightly on `main` after the shallow-writer
  fix merges shows no job loading a manifest that matches none of its build.
  Find the first run started after the fix's merge commit with `gh run list
  --workflow nightly.yml --branch main --event schedule`, list its jobs with
  `gh api --paginate repos/moq-dev/moq/actions/runs/<id>/jobs`, and fetch
  each job's log with `gh api repos/moq-dev/moq/actions/jobs/<job>/logs`.
  It clears only when every log was fetched, every job that ran the Rust
  cache logged an `mbx[cache]: object cache` line, and no log has "loaded,
  but none matched"; steps 2 to 4 follow. A failed fetch leaves it
  unchecked. A match means another cause: bring it to the maintainer before
  deleting anything.

## Related

- [moq.pro's B2 account move](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/b2-account.md) - the `us-east-005` account and its credits
