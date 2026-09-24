# [M] moq-json snapshot decode patches the typed value

## Goal

Decoding a `.json.z` snapshot frame applies its patch to the typed value
instead of merging into a `serde_json::Value` and deserializing again. At
4096 broadcasts the stats decode costs about 400k allocations per tick today;
it drops by more than 10x on a stats decode benchmark, with the same wire.

## Plan

- Land the benchmark first: recover the `.json.z` half of the stats
  benchmark from #3955's prototype commit `d86496be2` (1 to 4096 broadcasts,
  1 or 4 tiers, 100% or 10% changed per tick), which that PR does not merge.
  Record the baseline in the PR.
- Patch in place on the typed value where the shape allows it; fall back to
  the `Value` merge only for shapes that cannot be patched, and say which.
- Public API: none expected. Wire: none.
