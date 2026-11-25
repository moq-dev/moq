# [S] Verify the B2 Rust cache

## Goal

Confirm the B2 Rust cache earns its keep for the hosted Nightly jobs on
`main` (Linux, macOS and Windows), and record the numbers in the PR that
closes this:

- Nightly's Linux, macOS and Windows jobs hit what the previous Nightly
  wrote, and Windows keeps the hit rate it reached on 2026-11-14 once the
  download budget stopped binding.
- Restore plus build time on B2 (`us-east-005`) against the same jobs' last
  R2 runs (WNAM), and against the 150-210s Actions restore of the ~6 GB store
  the cache replaced, from the same commit where a quiet day gives one.
- Each object expires 30 days after upload (`infra/mbx/lifecycle.json`), so
  a crate no hosted job builds in that window stays cold for the next hosted
  job. Count how often a Nightly job pays that rebuild, including when
  `Cargo.lock` changed more than 8 times since the previous Nightly (see
  Plan).
- The bucket's size and monthly download bytes after its first 30 days,
  against B2's free egress (3x stored bytes, then $0.01/GB), so the cost
  once the Flamethrower credits end (about 2027-11-17) is known before then.

Use scheduled Nightly runs on `main` only (`gh run list --workflow
nightly.yml --branch main --event schedule`), never a dispatch on a branch.
The bucket starts empty, so count only Nightlies after the first that ran
on B2.

## Plan

Decided 2026-11-23 (maintainer, /quest-plan on the B2 move): ✅ rewritten
from the R2 verification for B2, whose API calls are free, so request counts
no longer matter; storage and egress do. The R2 history (expiry waves, the
download budget, per-job Linux baselines through 2026-11-14) is in this
file's git history as `quest/m0/ci/mbx-r2-verify.md`.

Decided 2026-11-24 (maintainer, /quest-run on
[moq-dev/moq#9558](https://github.com/moq-dev/moq/pull/9558)): ✅ after the
first warm scheduled Nightly on mbx 1.31.0, which ships
[jdx/mr-boxington#951](https://github.com/jdx/mr-boxington/pull/951), post
its per-OS cache summaries to
[jdx/mr-boxington#950](https://github.com/jdx/mr-boxington/discussions/950)
as the after-numbers for #951 (rejected: posting nothing upstream). If R2 had
no warm 1.31.0 Nightly before the move, use the second B2 Nightly and say
the remote changed. Reread that repository's AI policy before posting: its
CONTRIBUTING.md takes reports only from real use and requires AI disclosure,
and its AGENTS.md gives the disclosure line, which the poster appends
verbatim with its own runtime identifiers, followed by the `(Written by
<model> with <effort>)` line. Cite only logs and behavior; no source code.

Decided 2026-10-22 (maintainer): narrowed to hosted jobs. Since
[#6040](https://github.com/moq-dev/moq/pull/6040) the merge queue runs on the
self-hosted `moq-queue` runner with mbx's local store, so it neither reads
nor writes the bucket.

How mbx reuses a previous Nightly: it names a Cargo build's prediction
manifest by its workspace's `Cargo.lock` digest. When a lockfile has no
manifest yet, mbx inherits one from up to 8 earlier lockfile states, found
through Git history. Without a prediction, a compilation is "not looked up".
Nightly is the only hosted writer for Windows and macOS, so after more than 8
`Cargo.lock` changes the previous Nightly is out of reach. Decided 2026-10-29
(maintainer): accept that, and count it under the rebuild item in the Goal
before asking upstream for a deeper lookback.

What R2 settled, for reading the B2 numbers:

- Windows missed about 90% of looked-up keys until
  [#8631](https://github.com/moq-dev/moq/pull/8631) raised mbx's download
  budget to 20 GiB; the
  [2026-11-14 workspace build](https://github.com/moq-dev/moq/actions/runs/43601796176/job/132468015187)
  then hit 80% (621 hits, 156 misses, 390 not looked up, 13.5 GiB
  downloaded). `mbx analyze` lists every miss on a fresh runner as "misses
  with no earlier recording", so it names no cause.
- macOS reuses earlier Nightlies through inheritance (about 1,000 hits a
  night before the 2026-11-13 expiry wave).
- Linux got full history (`fetch-depth: 0`, `filter: blob:none`) in
  [#8342](https://github.com/moq-dev/moq/pull/8342). Not looked up fell to
  near 0, but hits stayed inside each job's depth-1 range while the 5 GiB
  budget bound. Still open: whether full history now raises hits above the
  depth-1 range. If not, plan a quest that returns those jobs to
  `fetch-depth: 1`, since full history then costs checkout time for nothing.
- A refused conditional put never refreshed an object on R2. B2 runs with
  `MBX_REMOTE_S3_CONDITIONAL_WRITES=off`, so a re-upload of an existing key
  writes a new version and restarts its 30 days. Note whether expiry waves
  like 2026-11-13's still appear.

Size and download bytes come from the B2 console's Reports page, read
through the maintainer's agent Chrome.

## Required

- [R2's Rust cache is torn down](/quest/m0/ci/mbx-b2.md) - lands only once a scheduled Nightly hits B2

## Related

- [Release pull requests read the B2 Rust cache](/quest/m3/mbx-release-cache.md) - the release branch's check, behind the cut
