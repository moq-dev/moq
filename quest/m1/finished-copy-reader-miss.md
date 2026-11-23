# [M] A reader of a finished relay copy misses history upstream still holds

## Goal

A reader attached to a relay copy whose publisher finished the track still gets every
group of its range that upstream holds, including ones the copy has evicted. The copy
answers from its cache only what it can prove upstream would answer, on every lite
version.

## Plan

Found while checking [moq-dev/moq#9377](https://github.com/moq-dev/moq/pull/9377).
Reproduced on lite-06 with a test like `lite06_fill_walk::a_recovery_outlives_the_finished_subscription`:

1. The publisher keeps groups at 0, 1 s, …, 5 s for good (unbounded pool). The relay copy
   receives them, then expires them while a reader keeps it live.
2. The publisher writes one more group and finishes the track, holding that group's
   stream. The copy's subscription FINs and its tail waits for that group
   (`ServeMode::Tail`).
3. Meanwhile a second reader asks for 1 s..4 s with no max delay, and a FETCH of 2 s
   follows.
4. Once the held group lands, the copy finishes. The FETCH goes out as one group and gets
   2 s, but the reader's range ends cleanly with only 2 s: 1 s and 3 s are given up.

Why: `ServeMode::Tail` never polls `dynamic`, so the reader's miss stays queued. At
`ServeEnd::Finished`, `dynamic.fetches_only()` drops queued misses ("a reader's miss
settles at once, as with no handler"), and `FinishedServe` serves only FETCHes. Its doc
says the tail already settled what the subscription owed. That holds for groups never
delivered, but not for history the copy received and then evicted. Upstream still holds
it, so answering "not found" breaks the cache rule in `AGENTS.md`. lite-07 shares
`FinishedServe` and `fetches_only`; check it too. JS: check `#runFills` and how a JS copy
ends.

Recommended: a finished copy keeps serving reader misses below the track's end the way
it serves FETCHes. A multi-group miss goes through one per-run recovery (`RangeFills` in
per-run mode, which works on any version), and a one-group miss through a FETCH. Drop
the `fetches_only` narrowing, or limit it to keys the tail settled without delivering
them. Start with a test that fails on `main`: the reader above gets 1 s, 2 s and 3 s.
