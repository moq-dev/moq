# [S] moq-net's ingest fuzz target finds no stall

## Goal

Nightly's moq-net `ingest` fuzz target runs its budget without a finding.
The stall it found on a complete 13-byte stream is fixed at its cause, and
that input is kept as a regression test.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)): milestone m0,
a child of the Nightly epic.

- The scheduled run of 2026-11-03
  ([job](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988510475))
  panicked at `rs/moq-net/src/fuzz/ingest.rs:298` with "the ingest stalled
  on a complete 13-byte stream". Its crash artifact is `crash-106ef498…`.
  The 2026-11-01 E0391 build failure noted in
  [the CI audit](/quest/m2/audit-ci.md) no longer occurs.
- Download the artifact before it expires, then replay it with
  `cargo fuzz run ingest <artifact>`. Decide whether the ingest really
  stalls on that input or the harness's stall check is wrong, and fix
  whichever it is.
- Add the input as a fixed regression test next to the target, so `just
  check` covers it without fuzzing.
- The same run's `Fuzz (ts)` and `Fuzz (annexb)` failures are media
  targets. They belong to [fuzz findings](/quest/m1/fuzz-findings/README.md),
  or else to the Nightly epic's own triage, not to this quest.

Public API: none expected. Wire: none expected.

## Related

- [Fuzz findings](/quest/m1/fuzz-findings/README.md) - the media fuzz targets' findings
