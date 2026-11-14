# [M] HoldCycle keeps a dead path from returning off its pinned seed

## Goal

`HoldCycle.cfg` holds `NoStaleReturn` under any simulation seed, as its
bounds promise: one session change and a patient timer, where the
[verify-route skill](/.claude/skills/verify-route/SKILL.md) says the rule holds.
Today Nightly simulates it only with `-seed 1`, which passes. Random seeds
break `NoStaleReturn` within a few hundred thousand states (a 35-step
behaviour). The base of
[the interest set](https://github.com/moq-dev/moq/pull/8728) breaks it the
same way, so no change to interest edits caused it.

## Plan

Recommended:

1. Rerun `HoldCycle.cfg` with `-simulate num=3000 -depth 100` and no seed
   until it fails, then read the behaviour as the skill's Reading a failure
   says. Decide whether the model misdescribes the router or the router lets a
   relay use a route through a hop it already learned died.
2. Replay it on `route::tests::Sim` under `Rule::PATH`. If the code follows
   the path, fix the code and pin the replay as a fixture. If it does not,
   fix the model.
3. Drop `-seed 1` from `HoldCycle.cfg`'s `Simulate:` line once random seeds
   pass, so Nightly samples new orderings each night. Do not drop
   `NoStaleReturn` from the config unless the behaviour shows a second session
   change that makes this `Twice.cfg`'s accepted case.
