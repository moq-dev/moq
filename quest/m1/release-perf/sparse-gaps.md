# [M] Sparse gaps

## Goal

On a track with many open gaps, recording an arrival and flushing gaps cost
O(log gaps). `track_sparse_read/timed` and `track_gap_flush` stop growing
faster than their size parameter.

## Plan

Found by reading code on 2026-10-21. Confirm each step with callgrind
instruction counts at two sizes before and after the change.

- `Ledger` (`rs/moq-net/src/model/ledger.rs`) stores its runs in a `Vec`.
  [#5802](https://github.com/moq-dev/moq/pull/5802) moved it from the session
  tail ([#4533](https://github.com/moq-dev/moq/pull/4533), which only ever
  held a few gaps) onto every track's live arrivals. Each arrival now pays
  O(gaps) twice. `account` uses `Vec::splice`: closing the lowest gap moves
  every run, about 100 KB at 4096 gaps. And while gaps exist, `expire` runs a
  `dedup_by` over all runs plus `snapshot_subscription`. Under the 3600 s
  delay in `track_sparse_read/timed`, gaps never fold, so a sparse feed is
  quadratic overall. Fix: key the runs in a `BTreeMap<start, (end, since)>`,
  as `Delivered` already does. Cache the earliest fold instant (the smallest
  `since` plus grace), so `expire` returns in O(1) when nothing is due.
- `track_gap_flush` (4.1x at 64 gaps, 96x at 4096): `Cursor::forget_past_budget`
  in `track.rs` loops once per gap. Each pass calls `delivered.forget_below`
  (a `BTreeMap::split_off` plus an insert) and reads the wall clock again in
  `state.aged()`. Fix: read `now` once. Every gap below the shared successor
  gets the same verdict, so look up the largest run start below it with
  `runs.range(..s).next_back()` and call `forget_below` once.
  `Delivered` comes from [#5446](https://github.com/moq-dev/moq/pull/5446) and
  the wall-clock check from [#5548](https://github.com/moq-dev/moq/pull/5548).
  Both are intended features, so only the cost per gap is wrong.

Release forgot nothing here because it had no per-reader delivery record.
Don't aim for its absolute numbers. Aim for flat cost per gap across the
`track_gap_flush` and `track_sparse_read/timed` sweeps (16, 256, 4096).
