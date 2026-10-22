# [M] Sparse gaps

## Goal

On a track with many open gaps, recording an arrival and flushing gaps cost
O(log gaps). `track_sparse_read/timed` and `track_gap_flush` stop growing
faster than their size parameter.

## Plan

The source changes in this PR replace the ledger's vector with a sequence-keyed
`BTreeMap` and an age-keyed `BTreeSet`. Splits inherit gap ages; merges and expiry
remove both index entries. In-order arrivals extend the existing run in place.
An age index, rather than only a cached minimum plus a full scan, also keeps an
arrival cheap when one older gap expires every time.

The cursor samples the clock once and finds the last delivered run below the
successor supplying the stale verdict. It calls `forget_below` once per shared
successor, preserving later gaps whose successor remains within budget.

### Remaining decision

The original goal says a gap flush costs O(log gaps). The implementation follows
the planned indexed lookup and single split, but dropping the retired map entries
necessarily costs O(retired entries). No teardown was deferred or moved outside
the operation. The bound is O(log gaps + retired entries) per shared successor;
the sparse arrival bound is O(log gaps) amortized, plus any gaps it expires.

1. Accept this bound for a batch, with instruction sweeps confirming no repeated
   work per gap (recommended).
2. Require the whole batch, including reclaiming retired storage, to be O(log
   gaps). That requires a different ownership/representation design; retain the
   source checkpoint while that scope is settled.

Do not finish this quest until the maintainer settles that interpretation and the
validation limits are resolved. The original Goal above is unchanged.

### Verification

`track_sparse_read` and `track_gap_flush` retain their existing nightly target.
The new `track_sparse_expire` workload opens one gap while one older gap expires,
swept over 16/256/4096 retained gaps. The ledger tests check both indices after
split/merge/expiry operations, demand reordering, a replaced leading run, grace
changes, and repeated individual expiry. Wall-clock and media-time batch tests
preserve the first gap with a fresh successor.

Profiling boundaries exclude fixture setup. Both builds warm the same initial
cache-queue capacity expansion before collection: a single iteration immediately
after a 4096-item setup otherwise measures an unrelated `VecDeque` growth copy.
The final counts measure steady-state arrival work; cold allocation/growth remains
an amortized production cost and is not claimed to be logarithmic per operation.

Callgrind 3.27.1 with Rust 1.98.1, optimized with symbols and line tables,
compares [c78547efff](https://github.com/moq-dev/moq/commit/c78547efff) plus the
identical benchmark fixture on both builds. One
`--bench --test --exact` iteration collects only `*read_pair`, `*Expiring*arrive`,
or `*GapFlush*flush`, excluding setup. The expiring workload warms two arrivals
because the eviction queue trails the arrival queue by one entry.

| Workload | Gaps | Before instructions | After instructions |
|---|---|---|---|
| read | 16 | 29,591 | 30,838 |
| read | 256 | 43,625 | 34,389 |
| read | 4096 | 315,641 | 40,856 |
| expire | 16 | 15,484 | 16,242 |
| expire | 256 | 23,780 | 17,791 |
| expire | 4096 | 138,439 | 22,050 |
| flush | 16 | 38,996 | 25,405 |
| flush | 256 | 443,152 | 49,921 |
| flush | 4096 | 9,106,805 | 463,636 |

At 4096 gaps, read-pair instructions fall 87.1%, steady expiry 84.1%, and flush
94.9%. At 16 gaps, the indices add 4.2% to a read pair and 4.9% to expiry; the
larger retained-gap costs no longer come from full ledger walks. Flush retains
linear storage reclamation rather than charging that work elsewhere.

Validation: all 2918 moq-net tests pass (eight ignored nightly tests). `just ci`
passes Rust lint/docs/wasm, quest validation, and drill applicability, then fails
on the unchanged Nix runner image because this host cannot write
`/proc/self/uid_map`. Broader test results remain recorded in the PR.

Found in the release comparison. The arrival ledger came from
[moq-dev/moq#5802](https://github.com/moq-dev/moq/pull/5802), delivery records from
[moq-dev/moq#5446](https://github.com/moq-dev/moq/pull/5446), and wall-clock aging
from [moq-dev/moq#5548](https://github.com/moq-dev/moq/pull/5548). Those features
remain; only their accidental repeated work changes.
