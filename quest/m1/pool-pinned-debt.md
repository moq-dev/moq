# [M] A pool held over capacity by its live edges still serves them

## Goal

A relay's cache pool whose target sits below what it may not evict (each track's latest group) keeps serving every track's live edge. A new subscriber receives the latest group, a catalog included, and no reader ends with `Lagged` (`TooFarBehind` on the wire) over a debt that evicting nothing more could ever pay. Losing older history to the pool stays the designed outcome.

## Plan

Reproduced against a relay whose headroom governor (`rs/moq-relay/src/cache.rs`, the default since moq-dev/moq#9269) targets zero: `cache.headroom = "100%"`, which is also where it lands on a host whose available memory is below the headroom.

- A `moq import` publisher on lite-07, then `moq export fmp4`: the export's `catalog.json` subscription either ends `too far behind` about 40 ms after the relay accepts it (the relay logs `subscribed error ... track=catalog.json err=lagged`) or delivers nothing for 10 s.
- RTMP play times out resolving the same catalog (`stream not available`).
- The relay logs tens of thousands of `fetch started` walking keys down one at a time (49,109 in one run, against a handful with the governor off), the walk [lite06-fill-walk](https://github.com/moq-dev/moq/pull/9355) plans to bound.
- With `headroom = "0"` and the same capacity, every one of these passes.

Mechanism, from reading the model: `cache::Reader::debt` (`rs/moq-net/src/model/cache.rs`) is forgiven only once `pool.used() <= capacity`. Each track's latest group is never evicted, so a target below their sum leaves the pool over capacity for good and the readers' debt never clears. `Record::settle` (`model/record.rs`) then ends every reader that `cache::Reader::ends` picks, which holds for any record billing more than `ENTRY_OVERHEAD` and at least its track's average. A fresh reader of a sparse-key track (a lite-07 catalog) qualifies at once.

First step: a model test that fails on `main`. A pool resized below its tracks' retained latest groups, then a new reader of a sparse-key track reads that track's latest group, which must succeed. A second reader asserts the older groups are gone, so the test pins only the live edge.

Recommended, not decided:

- Debt counts only what eviction can still reclaim: the amount over capacity, less the bytes pinned as latest groups. A pool at its irreducible usage then carries no debt, and readers end only when ending them frees room something else can use.
- Rejected: the governor clamping its target to the pinned bytes. It fixes the governor's case but not a static `capacity` below the same sum.

Public API: none. Wire: none.
