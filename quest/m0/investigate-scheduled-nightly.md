# [M] Investigate scheduled Nightly

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `scheduled Nightly`: failure

<!-- kixel.quest finding {"series":"scheduled Nightly","culprit":null} -->

- Series: [`scheduled Nightly`](https://github.com/moq-dev/moq/actions/runs/45242597151/attempts/1), in jobs.
- 2026-11-23: 19 jobs against a baseline of 0 jobs, +19 jobs, at [`b29b90b`](https://github.com/moq-dev/moq/commit/b29b90be2908b0e688ff969ff1c210c60d7e2f18). Run 45242597151 attempt 1 failed, failing Tests (test resume-model), Media (firefox player), Tests (rs quic-sim), Nightly (Relay session benchmarks), Nightly (Rust bench targets), Nightly (Rust shared bench targets), Nightly (Rust supplemental benchmarks), Audio quality, Nightly (Rust benchmark tests), Nightly (JS coverage), and 9 more.
