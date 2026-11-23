# [XS] The fan-out benchmark's session scenario runs on lite-07 and older bases

## Goal

`just bench-fanout` measures its `session` scenario against current `main`
and a base revision, as it does `model`, on every version it accepts, so a
fan-out cost change in the session layer shows up in its numbers.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8243](https://github.com/moq-dev/moq/pull/8243), whose
measurement could not run the scenario). Milestone: m4, a benchmark nobody
waits on; no Nightly job runs it.

The harness builds on `main` again and builds its origins with
`origin::Config::default()`, since a Relay ID can no longer be pinned
([moq-dev/moq#9467](https://github.com/moq-dev/moq/pull/9467)). What remains:

- With `moq-lite-07-wip` it hangs during setup, before any cycle (`fanout-session 1 0 moq-lite-07-wip`
  never exits, on `main` before #9467 too), while `moq-lite-06` runs. Find whether the
  harness (its hidden route, or the order it links sessions) or moq-net is at fault.
- The harness builds against each revision's own crates, so a base whose API
  differs from the harness's fails to build: either
  pick the spelling by what the revision exposes in `bench/run.sh`, or state the
  earliest base it measures.

Public API: none. Wire: none.
