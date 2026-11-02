# [XS] The paired bench baseline builds

## Goal

The scheduled Nightly's paired bench jobs build their baseline and get to the
comparison. Check with `gh run list --workflow nightly.yml --event schedule`
and the "Rust bench targets" step of the newest run (the other paired jobs,
such as "io_uring benchmarks", share the cause). Once a scheduled run gets past
the base build, delete this quest.

## Plan

Decided 2026-11-01 (maintainer, `/quest-plan`): a wait-and-check quest that
stays ready, since #7245 likely fixed it already and only the next scheduled
run can tell.

Scheduled run
[41204380716](https://github.com/moq-dev/moq/actions/runs/41204380716)
(2026-10-31) failed every paired bench job before comparing: `bench/run.sh`
checks the baseline out to `$RUN/base`, under `target/bench/nightly/` inside
the main checkout, and Cargo could not merge that checkout's
`.cargo/config.toml` `build-ffi` alias with the current one ("failed to merge
key `build-ffi`").

That run's head already had
[moq-dev/moq#7245](https://github.com/moq-dev/moq/pull/7245), which makes the
aliases strings, so a nested checkout's alias overrides instead of merging.
Its baseline (`previous-nightly`) predated #7245 and still had the list form.
The next scheduled run's baseline includes #7245, so it should build.

If it still fails on the merge, build the baseline outside the checkout (a
`git worktree` beside the repository instead of under `target/`), so no
enclosing `.cargo/config.toml` applies.

Found by [moq-dev/moq#7253](https://github.com/moq-dev/moq/pull/7253).
