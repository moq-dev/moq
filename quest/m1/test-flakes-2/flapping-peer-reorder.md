# [XS] Replay the flapping-peer reorder seed

## Goal

The drill `bursts_cross_a_flapping_peer::impaired` (`rs/moq-relay/tests/drills.rs`)
passes for seed `783778834340462316`, which failed with "the profile never
applied reorder" in run
[39292063137](https://github.com/moq-dev/moq/actions/runs/39292063137) before
[#5943](https://github.com/moq-dev/moq/pull/5943) merged.

## Plan

- Replay the seed on current main with `MOQ_SHAPER_SEED=783778834340462316`.
- If it no longer fails, record the replay in the PR and delete this quest.
- Otherwise fix it at the cause: either the profile should have applied
  reorder on that seed, or the shaper's impairment check is wrong for this
  drill.

Public API: none. Wire: none.
