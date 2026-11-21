# [S] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

Found by the 2026-11-20 triage of Nightly run
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952).
[moq-dev/moq#9294](https://github.com/moq-dev/moq/pull/9294) killed or excluded
26 of its 30 survivors. Four remain in `resume.rs`:

- `unraised`: `start < floor` to `<=`, and the `end < floor` guard as `true`
  or `<=`. They decide only an anchor exactly at the resume point, or a time
  end above it; probes of both, with the replacement holding and lacking the
  floor group, read the same groups.
  [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278) deletes
  `unraised` for `raised`, which keeps only the `time < floor` guard: once it
  lands, rerun `cargo mutants -f rs/moq-net/src/model/resume.rs --re raised`
  and work from what survives.
- `Reader::sync`: `delete !` in `if !matches!(change, Change::Scalars)`, the
  resolve that takes a ready copy's cache resolution right after a range
  change. Skipped, `Reader::window` placed the same groups in every case
  tried (an edit or a whole update under the resume point, off a failed copy,
  and through an unanswered replacement), since a backfill copy asked from
  below the floor places a time start under it. Find the ordering where only
  this resolve places a range right (the `verify-resume` model may show it),
  or show it only saves work and exclude it in `.cargo/mutants.toml`.

Line numbers drift; rerun `cargo mutants -f <file>` on current main first.

Public API: none. Wire: none.
