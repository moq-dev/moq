# [M] Investigate scheduled Smoke

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `scheduled Smoke`: failure

<!-- kixel.quest finding {"series":"scheduled Smoke","culprit":null} -->

- Series: [`scheduled Smoke`](https://github.com/moq-dev/moq/actions/runs/44921862668/attempts/1), in jobs.
- 2026-11-25: 1 jobs against a baseline of 0 jobs, +1 jobs, at [`f5f8fed`](https://github.com/moq-dev/moq/commit/f5f8feded4150c7a0c6ccb33187089ec703661d3). Run 44921862668 attempt 1 failed, failing Cloudflare interoperability.
