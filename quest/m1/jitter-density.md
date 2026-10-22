# [M] The jitter estimator costs constant time per frame

## Goal

The arrival-jitter estimator in `js/hang` and `rs/moq-audio` does amortized
O(1) work per observed frame however densely a publisher packs media
timestamps, and produces the same target series for ordinary traces. A
publisher can no longer stall a viewer's main thread by sending many frames a
microsecond apart.

## Plan

Found in a 2026-10-22 media audit at
[main 1f5caa7](https://github.com/moq-dev/moq/commit/1f5caa7026bcfd8a2684da3712b384b5f74a7496)
and confirmed by source at
[main d4cdfe4](https://github.com/moq-dev/moq/commit/d4cdfe4368b17cbdf7d2a0c43d24e364a9626480).
`Jitter.observe` in `js/hang/src/container/jitter.ts` and `Estimator::observe`
in `rs/moq-audio/src/jitter.rs` keep every arrival inside a 2 s media-time
window and scan it on each observation for the fastest reference, so N frames
in one window cost O(N^2). The audit measured the JS estimator at 82 ms for
10,000 observations a microsecond apart and 4.51 s for 80,000; Rust was
source-verified only. Observation runs before decode or drop, so nothing
downstream bounds it.

For an in-order frame the reference is the window's minimum of
`arrival - media`, which a monotonic deque gives in amortized O(1). A
reordered frame needs the minimum over the prefix at or before its timestamp,
which the deque no longer holds; measuring it against the window minimum
instead reads a reordered frame's delay as at least as large, which is
arguably the wait it really costs. Decide that against the conformance
corpus, and update `doc/concept/audio-jitter.md` (normative) in the same PR
if the definition changes. Both languages land together and keep passing the
shared corpus.

Add a benchmark swept over observations per window (ordinary 20 ms cadence up
to microsecond spacing) in both languages, so a slope shows up. No API or
wire change.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the estimator's owner and conformance corpus
- [Audio playout memory](/quest/m1/audio-playout-memory.md) - the memory side of the same hostile-publisher concern
