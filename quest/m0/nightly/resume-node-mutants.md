# [S] Mutants in resume and route node code are killed

## Goal

Nightly's mutants jobs report no survivor in `model/resume.rs` or
`route/node.rs`: each is killed by a test, or excluded in
`.cargo/mutants.toml` with the reason it is equivalent.

## Plan

Found by the 2026-11-19 triage of run [44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083), in code
that landed after the resume mutants child closed with
[moq-dev/moq#9144](https://github.com/moq-dev/moq/pull/9144):

- `MISSED resume.rs:193 match guard ... Producer::subscription`
- `MISSED resume.rs:1400 delete ! in Reader::sync`
- `MISSED route/node.rs:440 delete ! in Node::release_orphans`

[moq-dev/moq#9115](https://github.com/moq-dev/moq/pull/9115) landed resume
tests after the run started; first check which survivors its tests already
kill.

Caused by:
[moq-dev/moq#9127](https://github.com/moq-dev/moq/pull/9127) and
[moq-dev/moq#9134](https://github.com/moq-dev/moq/pull/9134) (resume.rs),
[moq-dev/moq#9106](https://github.com/moq-dev/moq/pull/9106) (node.rs).

Public API: none. Wire: none.
