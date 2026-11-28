# [M] Investigate scheduled TS / TS validation

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `scheduled TS / TS validation`: failure

<!-- kixel.quest finding {"series":"scheduled TS / TS validation","culprit":null} -->

- Series: [`scheduled TS / TS validation`](https://github.com/moq-dev/moq/actions/runs/46165717865/attempts/1).
- 2026-11-28: 1 against a baseline of 0, +1, at [`6d58a45`](https://github.com/moq-dev/moq/commit/6d58a45516c365ca0657354a1895675e7b751c9f). Failed in run 46165717865 attempt 1; no run in the window before passed it.
