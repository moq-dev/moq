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

- Series: [`scheduled Nightly`](https://github.com/moq-dev/moq/actions/runs/45035580806/attempts/1), in jobs.
- 2026-11-25: 24 jobs against a baseline of 0 jobs, +24 jobs, at [`628ff81`](https://github.com/moq-dev/moq/commit/628ff8199d81c08d381f70382df0b2a3e487021c). Run 45035580806 attempt 1 failed, failing quic-interop / moq-quic and quic-go, Media (firefox player), quic-interop / moq-quic and quinn, Tests (rs quic-sim), Tests (rs cli-export), Nightly (Rust bench targets), TLA+ (rest), Tests (test route-sweep), Nightly (Rust shared bench targets), Nightly (JS benchmarks), and 14 more.
