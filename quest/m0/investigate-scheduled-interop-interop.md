# [M] Investigate scheduled Interop / Interop

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `scheduled Interop / Interop`: failure

<!-- kixel.quest finding {"series":"scheduled Interop / Interop","culprit":null} -->

- Series: [`scheduled Interop / Interop`](https://github.com/moq-dev/moq/actions/runs/46166329015/attempts/1).
- 2026-11-28: 1 against a baseline of 0, +1, at [`9a20cdf`](https://github.com/moq-dev/moq/commit/9a20cdf5bcb9788813cdfcd9ed9ceb3008b08c64). Failed in run 46166329015 attempt 1; no run in the window before passed it.
