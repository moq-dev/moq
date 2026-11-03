# [S] Release's MKV importer bounds what it buffers

## Goal

On `release`, an MKV input that declares a huge element and trickles its
bytes no longer grows the importer's memory without limit: once the bytes
held for one incomplete element pass a size bound, the import fails with an
error. Ordinary files of any total size still import, and the published API
and supported inputs are unchanged.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234).

Backport #7234's MKV unbounded-buffer fix to `release`? ✅ Trimmed
backport, m0: confirm with the 1 GiB repro on `release`, size bound only.

Why (the recommendation's reasoning, from #7234's Impact): #7234 is a real memory-exhaustion fix, but the full change is a
rewrite that also refuses inputs `release` accepts today (subtitle tracks,
CMAF output, video `BlockGroup`s with fields after their `Block`). A size
bound was proposed to fix the exhaustion without changing supported inputs.
The investigation below disproves that premise for a blanket scratch cap;
implementation waits for a course decision rather than adding a refusal.

Investigation 2026-11-03, on
[release 338cb9b3e8](https://github.com/moq-dev/moq/commit/338cb9b3e82f2236d8611e682190e946077efd7b):

- **Reproduced**: ported
  `ignored_payload_does_not_accumulate_under_a_large_metadata_master` from
  [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234). A 1 GiB
  declared EBML master containing a nearly 1 GiB Void, fed in 1 KiB chunks,
  fails the bounded-retention assertion with `ignored bytes retained: 1030`.
  A diagnostic continuation measures 32,774 bytes after 32 chunks and 65,542
  after 64. This declares 1 GiB; it does not allocate or feed 1 GiB.
- **Transport bound**: `moq_net::group::MAX_CACHE_BYTES` is 33,554,432 bytes
  in `rs/moq-net/src/model/group.rs`. `create_frame` and `write_frame`
  enforce it, including output container overhead. It bounds a published
  frame, not an MKV master or ignored input.
- **Supported-input counterexample**: complete WebM files containing a VP8
  track and frame, with an EBML Void payload of 33,554,431 or 33,554,433
  bytes, both decode and finish successfully when fed in 1 KiB chunks.
  Thus the transport limit cannot be used as a blanket element cap.
  ebml-iterable 0.7.1 already defaults to a 4,000,000,000-byte sized-tag
  limit; increasing a new cap to that ceiling does not solve the reproduced
  memory exhaustion. Its setter also limits sized Segments and Clusters,
  so lowering it would restrict total file and cluster sizes.
- **Two owners, not one scratch buffer**: `Import::decode` retains an
  incomplete leaf in `self.buffer`. The decoder separately captures
  completed children of `Ebml`, `Info`, `Tracks`, `TrackEntry`, `Audio`,
  `Video`, and `BlockGroup` in its private `OpenTag.children` vectors until
  the master completes. Feeding 64 complete 1 KiB Void children under the
  same 1 GiB master leaves scratch length zero, with decoder position
  66,121. `TagDecoder::deliver` still pushes every child into the captured
  master. A scratch-length check misses that memory, including per-child
  allocation overhead. Removing the capture list alone still buffers leaves;
  0.7.1 offers no skip/stream hook.
- No production code, API, wire, dependencies, or versions changed. No
  release implementation PR exists. The temporary diagnostic probes were
  removed after running them; they must not become tests that require the
  vulnerability to persist. The main bookkeeping attempt is
  [moq-dev/moq#7718](https://github.com/moq-dev/moq/pull/7718).

After the course decision, remaining work is the release fix, bounded
retention and accepted-input boundary/large-file coverage, release checks,
semver verification, and local `/quest-review`. Preserve CMAF, skipped track
roles, and video BlockGroups with trailing fields; do not blindly cherry-pick
the full breaking rewrite. Version bumps and publication remain out of scope.

Public API: none; if refusing needs a new `mkv::Error` variant on
`release`, report it, since the enum is published. Wire: none.

## Waiting

- Maintainer: approve replacing the size-bound-only plan with a support-preserving
  incremental parser backport (recommended), or defer the patch until a release
  cut containing the main fix. The former must discard ignored leaves and
  metadata children as they arrive and bound retained block/config data by
  actual output ownership, while keeping release's accepted BlockGroup layouts
  and CMAF output. A new blanket refusal conflicts with both supported-input
  preservation and the repository's bounded-ownership rule. Clear this wait
  when the chosen course is recorded here or on
  [moq-dev/moq#7718](https://github.com/moq-dev/moq/pull/7718).

## Related

- [Backport the relay and auth hardening to release](/quest/m0/backport-release-hardening.md) - other security backports for the same patch release
