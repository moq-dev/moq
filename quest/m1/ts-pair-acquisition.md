# [M] TS pair validation distinguishes acquisition from persistent misalignment

## Goal

The late-joining pair test accepts supported clean acquisition and detects
persistent table misalignment without mistaking interleave for emission time.
Its positive baseline and negative controls run in CI before the scheduled
hard gate in #5328 is merged.

## Plan

Decided with the maintainer on 2026-10-11: fix the finding, not just diagnose
it. Keep this separate from initial catalog routing and the broader redundant
TS coordinate/API planning in #5327. Fix the baseline before #5328 merges;
do not temporarily make the pair test non-gating.

The validation run in #5328 used `just test ts --pair`: two exports of one
45-second generated broadcast, the second joining five seconds late. Both
exporters and the publisher exited successfully. PAT/PMT agreement was 100%;
SDT had 20 common placements out of 23 in the union, or 86.96%, below the
existing 90% gate over 41 seconds of shared media. The mismatches were near
acquisition; 20 consecutive later SDT placements matched. These observations
do not establish permanent phase drift.

`test/ts/table-anchor.py` tags a table with the next PES PTS and infers a
timer-driven phase difference from median cadence. Check those assumptions
against the current slot scheduler and the capture's actual table placement.
Correct the grader, the mux, or both as the evidence requires; do not assume
the exporter is defective because that diagnostic says so.

Define and document the acquisition behavior being graded for the generated
profile. Any distinction between acquisition tables and recurring tables must
follow an observable stream condition with automated evidence. Do not ignore
an arbitrary number of seconds, reduce the agreement or coverage thresholds,
or shrink the shared window to the surviving table emissions. Keep missing
expected tables and inadequate evidence distinguishable from a passing check.

Add deterministic controls for supported late acquisition and changed PES
interleave, plus persistent phase offset, drift and table loss within shared
media. The controls must fail for their intended reasons and must prevent a
grader that accepts everything from passing. Keep table carriage, cadence and
packet identity separate: this quest does not promise byte-identical TS legs,
introduce `--sync`, or claim ST 2022-7 compatibility.

Run the short controls on relevant PRs and the real pair arm at least nightly,
using shared `just test` recipes. Preserve captures and reports on failure.
Require a passing clean pair baseline and failing negative controls before
unblocking #5328; a green PR run that skips the pair arm is not that evidence.
Update `test/ts/README.md` and misleading grader comments/diagnoses inline.

Public API and wire: none expected. New shared-coordinate or synchronization
contracts remain with the broader alignment planning.

## Related

- [Validation PR #5328](https://github.com/moq-dev/moq/pull/5328) - its scheduled pair gate must wait for this fix
- [Pair gate review](https://github.com/moq-dev/moq/pull/5328#discussion_r4243103700) - a known-failing baseline would generate daily alerts
- [TS roadmap #5327](https://github.com/moq-dev/moq/pull/5327) - owns the broader redundant alignment planning
- [Deterministic TS legs](/quest/m2/ts-hitless.md) - packet identity requires a separate contract and proof
- [Initial catalog routing](/quest/m1/initial-catalog-routing.md) - distinct acquisition failure found by the same validation work
