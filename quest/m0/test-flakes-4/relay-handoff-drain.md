# [S] A new relay takes over from a draining one every run

## Goal

`moq-relay`'s integration test
`handoff::a_new_relay_takes_over_while_the_old_one_drains`
(`rs/moq-relay/tests/integration/handoff.rs`) passes every run, alone and in a
loaded full run, fixed at its cause.

## Plan

Seen failing once on 2026-11-24 in a full local run, by the agent iterating
[moq-dev/moq#9542](https://github.com/moq-dev/moq/pull/9542), which didn't
investigate it; the maintainer chose to file it. No failure message was kept,
so start by reproducing: run the test alone in a loop through `quest gate`,
then under a loaded `cargo nextest` run of `moq-relay`, and record the rate
and the failing assertion here. If it does not reproduce in a few thousand
runs, check Nightly and merge queue history for it before deleting this
quest as not reproducible.
