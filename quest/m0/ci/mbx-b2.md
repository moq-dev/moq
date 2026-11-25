# [M] The Rust cache moves to Backblaze B2

## Goal

Hosted Rust-cache jobs read and write a B2 bucket in the `us-east-005`
account, whose Flamethrower credits run to about 2027-11-17, instead of R2's
`mbx-moq-dev`. Objects expire 30 days after upload. After the first Nightly
rebuilds the empty bucket, the second scheduled Nightly hits at least as well
as the last one on R2. R2's `mbx-moq-dev` and the `MBX_R2_*` tokens are
deleted.

Non-goals: moq's other R2 buckets, moq.pro's tofu state (B2 rejects
conditional writes with 501), and the self-hosted runners' local store.

## Plan

Decided 2026-11-23 (maintainer, /quest-plan):

- ✅ Move mbx and [nix.moq.dev](/quest/m0/ci/nix-b2.md) (rejected: every R2
  bucket but moq.pro's tofu state; staying on R2). They hold about 95% of
  moq's R2 bytes: `mbx-moq-dev` 419 GB and the two nix buckets 75 GB on
  2026-11-23. R2 costs about $45 a month, mostly mbx's requests (18.0M
  `GetObject` and 4.9M `PutObject` from 2026-10-16 to 2026-11-07). B2 API
  calls are free, and the credits cover storage.
- ✅ 30-day expiry (rejected: 14 days; 60 days, little more hit rate since
  `Cargo.lock` churn invalidates most entries sooner). The 7-day rule was set
  for R2's cost. At 30 days the bucket was headed for about 1.65 TB, about
  $11.50 a month at B2 list once the credits end.
- ✅ Start the B2 bucket empty (rejected: copying R2's 419 GB, which all
  expires within 7 days and would get a fresh 30-day clock on B2).
- ✅ Delete the R2 bucket and revoke its tokens in this PR once a Nightly hits
  B2 (rejected: keeping them a week as a fallback).
- ✅ Rewrite [mbx-r2-verify](/quest/m0/ci/mbx-r2-verify.md) to measure B2,
  with `quest mv` to a B2 name, and update
  [mbx-r2-release-cache](/quest/m3/mbx-r2-release-cache.md) to match. The
  failed-puts work this was to fold in already ended: its draft
  [moq-dev/moq#8763](https://github.com/moq-dev/moq/pull/8763) closed and
  [moq-dev/moq#9558](https://github.com/moq-dev/moq/pull/9558) deleted its
  quest when bumping mbx to 1.31.0.
- ✅ An agent creates the bucket and keys in the B2 web console through the
  maintainer's signed-in agent Chrome (rejected: minting them through the B2
  API with the master key from Bitwarden's Break-glass project; a standing CI
  admin key in Deploy; the maintainer creating them by hand). If the console
  asks for a sign-in or 2FA, ask the maintainer. Each key goes straight into
  its GitHub secret, with no Bitwarden copy: to rotate, create a new key and
  replace the secret. `infra/README.md` records that.

Recommendations:

- Set `MBX_REMOTE_S3_CONDITIONAL_WRITES=off`. mbx 1.30.0's default `auto`
  only drops the headers after B2's first 501. Either way, only the task
  action manifest loses compare-and-swap, so concurrent writers can overwrite
  each other's prediction updates.
- Bucket-restricted keys: read-only as the repository's pair, and read plus
  write in `cache-write`, as the R2 pair is split today. Rename the secrets to
  `MBX_B2_*`.
- Check in the lifecycle in B2's format: hide 30 days after upload, delete
  hidden versions a day later, and cancel unfinished large files after a day
  (R2's rule aborts multipart uploads after a day).
- B2 egress to GitHub runners is free up to 3x stored bytes, then $0.01/GB,
  which the credits cover. Record the bucket's monthly download bytes in the
  verify quest, so the cost after the credits is known before they end.
- Compare a hosted job's restore time against its R2 runs, since the bucket
  moves from R2's WNAM to `us-east-005`.
- Update everything that names R2 for mbx: `.github/actions/rust-cache`,
  `infra/mbx/`, `infra/README.md`, the
  [CI README](/quest/m0/ci/README.md)'s caches bullet, and
  [read-only-secrets](/quest/m0/ci/read-only-secrets.md)'s `MBX_R2_*` line.

Public API: none. Wire: none.

## Related

- [nix.moq.dev moves to B2](/quest/m0/ci/nix-b2.md) - same account, console session, and R2 teardown
- [moq.pro's B2 account move](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/b2-account.md) - the `us-east-005` account and its credits
