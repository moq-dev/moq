# [S] The Rust cache stops paying for uploads R2 refuses

## Goal

`mbx-moq-dev` takes measurably fewer Class A requests per scheduled night,
mostly by failing fewer conditional puts with HTTP 412. Each hosted Rust-cache
job keeps or raises its hit rate. A root fix is proposed upstream in
jdx/mr-boxington so that mbx stops issuing puts it can tell will fail.

## Plan

Found 2026-11-13 in [#8631](https://github.com/moq-dev/moq/pull/8631), from
the bucket's per-object request analytics and each job's mbx summary for
scheduled Nightly run
[43250182795](https://github.com/moq-dev/moq/actions/runs/43250182795)
(2026-11-12, the first night with only scheduled writers). That night's
Nightly, CodSpeed, Interop and TS took 163,302 puts, 151,043 of them 412.
Nightly bursts on the earlier nights since the 14-day expiry ran 143k to 158k
412s each.

- 99% of the 412s hit objects stored before that night, so jobs racing to
  create the same object is not the cause.
- mbx caps one build's remote downloads at its action store's size budget,
  `gc.max_size`, which a hosted disk scales to 5 GiB on Linux and 10 GiB on
  Windows. 39 of the 41 Rust-cache jobs reached it, mostly on prefetch: the
  prediction manifest is keyed by the workspace's `Cargo.lock`, so every job
  on a platform prefetches from one shared set. Past the cap a restore fails
  as "cached action is missing blob", the unit compiles again, and mbx
  re-publishes the identical result: every blob and the action result, each
  a conditional put. All 128 blobs the doctest job reported missing were in
  the bucket, and their only puts that night were 412s.
- The budget refused 0.6 to 11.75 GiB more per job. macOS, whose default
  budget it never reached, logged no refusals and hit the most.

Decided 2026-11-13 (maintainer, on #8631): ✅ lift the download budget only,
as #8631 does with `MBX_GC_MAX_SIZE=20GiB` on hosted runners, the most any
job asked for (rejected: lifting it and also cutting writers; cutting writers
only, as first planned that day). Fewer writers would leave the refused
restores in place and stop storing the read-only jobs' own units. Whether to
cut writers is judged again once the remaining 412s are measured (below).

Decided 2026-11-13 (maintainer, on #8631): ✅ post the drafted report
upstream (rejected: not posting). Posted as
[jdx/mr-boxington#950](https://github.com/jdx/mr-boxington/discussions/950)
after its CONTRIBUTING.md and AGENTS.md, re-read that day, allowed
AI-assisted Discussions from real use. It asks mbx to name a budget refusal
as such, not to re-send objects of a result the remote already holds, and to
keep speculative prefetch from spending the budget the build's own restores
need. Add the after numbers there once measured.

Measure the first scheduled nights after #8631 against 2026-11-12. The
7-day expiry was applied that same day, after the 14-day rule of 2026-11-07,
so the baseline night may predate its first deletions while every later
night runs under it; note that beside the comparison:

- Class A requests by action and response status, and 412s, per hour, from
  Cloudflare's GraphQL `r2OperationsAdaptiveGroups` filtered on `bucketName`,
  with the maintainer's wrangler login as
  [mbx-r2-verify](/quest/m0/ci/mbx-r2-verify.md) does. Per-object counts
  (`objectName`) need one-minute windows to stay under the 10,000-row limit.
- Each Rust-cache job's hits, misses and not looked up, and any "download
  budget exceeded" or "cached action is missing blob" warnings, from its log.
- Disk: no hosted job fails for space with the larger store.

Measured 2026-11-14 for scheduled Nightly
[43601796176](https://github.com/moq-dev/moq/actions/runs/43601796176), the
first night after #8631 and the first under the 7-day expiry, over the same
12-hour window as the baseline (all four writer workflows):

| Night      | PutObject 412 | PutObject 200 | GetObject 200 | GetObject 404 |
| ---------- | ------------- | ------------- | ------------- | ------------- |
| 2026-11-12 | 151,043       | 12,259        | 317,569       | 6,534         |
| 2026-11-14 | 86,387        | 44,906        | 429,428       | 17,783        |

- Puts fell 20% (163,302 to 131,293) and 412s 43%. The new 200s and 404s
  are most likely the 7-day expiry deleting what the night then re-stored.
- Hits rose in 24 of the 38 Rust-cache jobs, often doubling (doctest 165
  to 421, Rust benchmark tests 156 to 638), and misses fell in 29. Hits fell
  in 12, mostly where hundreds of units were "not looked up" (features 590,
  x11 648, moxygen 342, Rust bench targets 321): mbx found no prediction or
  prior inputs for them, which the budget does not explain. Find why before
  judging hit rates on the second night. The Windows `cargo check` job went
  from 92 to 621 hits, as posted on
  [jdx/mr-boxington#950](https://github.com/jdx/mr-boxington/discussions/950).
- 20 Linux jobs still downloaded the full 20 GiB, nearly all of it prefetch
  (about 600 objects), and 25 still logged "cached action is missing blob"
  (mbx stops at 128 per job). The budget still binds on Linux, so these jobs
  keep re-uploading recompiled units.
- No hosted job failed for disk space with stores up to 21.7 GiB.
- Upstream merged
  [jdx/mr-boxington#951](https://github.com/jdx/mr-boxington/pull/951) on
  2026-11-13, unreleased as of 2026-11-14: prefetch stops at half the budget
  and a refused restore names the budget. It skips the HEAD-before-PUT check
  until the 412 counts after it are posted. Its adoption is
  [its own quest](/quest/m0/ci/mbx-prefetch-cap.md); raising the budget past
  20 GiB first was not tried, since no job records its free disk.

If 412s stay high once restores succeed, size the remaining sources (repeat
stores of units mbx did not look up, and blobs every job's session sends once)
and judge again whether cutting writers is worth the hits it would cost. Plan
the mbx bump that adopts an upstream fix as its own quest.

Public API: none. Wire: none.

## Waiting

- GitHub Actions: the 2026-11-15 scheduled Nightly, the second after #8631,
  confirms the 2026-11-14 numbers above.
- jdx/mr-boxington: a release containing #951, which
  [mbx-prefetch-cap](/quest/m0/ci/mbx-prefetch-cap.md) adopts before the
  remaining 412s are judged.

## Related

- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - its Windows misses may share this cause, since Windows also reached its 10 GiB budget
- [Adopt mbx's prefetch cap](/quest/m0/ci/mbx-prefetch-cap.md) - the upstream fix for the budget that still binds on Linux
