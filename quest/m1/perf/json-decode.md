# [M] moq-json snapshot decode patches the typed value

## Goal

Decoding a `.json.z` snapshot frame applies its patch to the typed value
instead of merging into a `serde_json::Value` and deserializing again. At
4096 broadcasts the stats decode costs about 400k allocations per tick today;
it drops by more than 10x on the stats benchmark #3955 added, with the same
wire.

## Plan

- Profile the current decode on that benchmark first and record the baseline
  in the PR.
- Patch in place on the typed value where the shape allows it; fall back to
  the `Value` merge only for shapes that cannot be patched, and say which.
- Public API: none expected. Wire: none.
