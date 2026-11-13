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

#8631 sets `MBX_GC_MAX_SIZE=20GiB` on hosted runners, the most any job asked
for, instead of the writer cut planned 2026-11-13: fewer writers would leave
the refused restores in place and stop storing the read-only jobs' own units.
Upstream, mbx should not spend the build's budget on speculative prefetch
ahead of restores the build needs, and should not re-send objects of a result
it just read from the remote. Its CONTRIBUTING.md and AGENTS.md, re-read
2026-11-13, allow AI-assisted Discussions with a disclosure line and accept
only reports from real use. #8631's body holds the drafted Discussion.

Measure the first scheduled nights after #8631 against 2026-11-12, under the
same lifecycle window (note [cache-expiry](/quest/m0/ci/cache-expiry.md) if
it applies in between):

- Class A requests by action and response status, and 412s, per hour, from
  Cloudflare's GraphQL `r2OperationsAdaptiveGroups` filtered on `bucketName`,
  with the maintainer's wrangler login as
  [mbx-r2-verify](/quest/m0/ci/mbx-r2-verify.md) does. Per-object counts
  (`objectName`) need one-minute windows to stay under the 10,000-row limit.
- Each Rust-cache job's hits, misses and not looked up, and any "download
  budget exceeded" or "cached action is missing blob" warnings, from its log.
- Disk: no hosted job fails for space with the larger store.

If 412s stay high once restores succeed, size the remaining sources (repeat
stores of units mbx did not look up, and blobs every job's session sends once)
before reconsidering fewer writers. Plan the mbx bump that adopts an upstream
fix as its own quest.

Public API: none. Wire: none.

## Waiting

- Maintainer: approve posting the Discussion drafted in
  [#8631](https://github.com/moq-dev/moq/pull/8631)'s body to
  jdx/mr-boxington, since each post outside the maintainer's orgs needs their
  approval. An agent then posts it, adding the after numbers, and links it
  here.
- GitHub Actions: the first two scheduled Nightly runs after #8631 merges
  supply the after numbers above.

## Related

- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - its Windows misses may share this cause, since Windows also reached its 10 GiB budget
- [Supplemental caches expire sooner](/quest/m0/ci/cache-expiry.md) - the storage side of the same bill
