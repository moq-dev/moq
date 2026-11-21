# [S] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

Found by the 2026-11-20 triage of Nightly run
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952).
[moq-dev/moq#9294](https://github.com/moq-dev/moq/pull/9294) killed or excluded
27 of its 30 survivors. Three remain in `resume.rs`, in `unraised`: `start < floor`
to `<=`, and the `end < floor` guard as `true` or `<=`. They decide only an anchor
exactly at the resume point, or a time end above it; probes of both, with the
replacement holding and lacking the floor group, read the same groups.
[moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278) deletes `unraised` for
`raised`, which keeps only the `time < floor` guard: once it lands or closes, rerun
`cargo mutants -f rs/moq-net/src/model/resume.rs --re raised` and kill or exclude
what survives.

[moq-dev/moq#9276](https://github.com/moq-dev/moq/pull/9276) resumes every reader
at the switch's boundary, the largest group held. So
`the_fronts_subscription_resumes_at_the_boundary_whichever_reader_is_behind`
(formerly `..._past_the_reader_furthest_behind`) no longer tells readers apart,
and `Resume::add`'s `at <= next` may survive again. To reach the comparison, both
readers need a held head at their boundary: `held_at` returns `None` for a reader
that handed out a group past every group its copies hold, and `Resume::add` then
takes `Whole` first. So readers' positions differ only when their copies differ,
such as one still holding a replaced or backfill copy with a later group, or more
of the boundary group's frames. Kill it with two such readers, or exclude it as
equivalent if no such pair is reachable.

Line numbers drift; rerun `cargo mutants -f <file>` on current main first.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278) lands or
  closes; check with `gh pr view 9278 --json state`.
