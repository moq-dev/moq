# [S] Nightly passes on main

## Goal

The scheduled Nightly on `main` passes end to end, so a red run means a new
break instead of noise. Nightly is the backstop for what the PR gate skips on
purpose (benches, examples, feature shapes; see
[moq-dev/moq#5531](https://github.com/moq-dev/moq/pull/5531)), and it has
failed every night since at least 2026-10-19, which let a bench and two
example breaks reach `main` unnoticed.

## Plan

Decided 2026-10-24 (maintainer), from a triage of run
[40090959108](https://github.com/moq-dev/moq/actions/runs/40090959108): one
child per distinct cause, each landing on its own. The PR gate keeps
skipping benches and examples; a green Nightly catches them within a day.

Decided 2026-10-27 (maintainer), from the coverage audit's triage of run
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673): more
children for the causes nothing owned.

The Features recipe and its nested checks now run independent commands and
report every failure before exiting nonzero. The shared runner announces each
command so an error can be tied to its check. Audio quality's independent benchmarks
and playback checks also continue after a failure, provided setup succeeded;
cancellation still stops them. The benchmark groups already collect failures.

Remaining work:

- Apply the same continuation to Platform's independent checks. The scheduled
  run below failed its macOS Rust step and skipped Swift, OBS, and installer
  checks. Coordinate with the active
  [hakari work](/quest/m0/ci/hakari.md), which owns release/platform jobs; this
  attempt leaves those files untouched to avoid conflicting edits.
- Confirm a scheduled run passes after the fixes land. Use scheduled runs on
  `main` only; do not dispatch Nightly. The latest inspected run,
  [41483654426](https://github.com/moq-dev/moq/actions/runs/41483654426)
  (2026-11-02, `dde50c699f69507ced243754c2f700a6184d1576`), failed and predates
  this attempt. Features stopped at the coroutine check, hiding every check
  after it. Its failure is not evidence against these continuation changes.
- Triage the remaining failures against current source and ownership before
  changing them. That run also failed Firefox media, the worklet matrix
  (FFmpeg source download), Rust and JS audits, Miri (missing Rust sources),
  ingest fuzzing, and shipping sizes. Its gate's close-drain failure belongs to
  [tokio close-drain](/quest/m0/test-flakes-2/tokio-close-drain.md). The relay
  session benchmark's `Evicted` failures stay with the active
  [fastest-reader work](/quest/m1/unbounded-groups/fastest-reader.md).

Failures already owned elsewhere stay there: the audio near-zero p95 row belongs
to [audio jitter target](/quest/m1/audio-jitter-target/README.md), the macOS
`receive_credit` timeout to [test flakes](/quest/m0/test-flakes-2/README.md), and
the paired bench threshold to [bench CI](/quest/m1/bench-ci.md).

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: coordinate Platform continuation with the active hakari owner
  before editing those jobs, then inspect a scheduled Nightly on `main` after
  the fixes land. Completion needs a green scheduled run; run 41483654426 is
  red and predates this attempt. Keep owned failures with the quests above.
