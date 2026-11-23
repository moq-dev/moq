# [M] Investigate bugs/escaped/found:unknown (count)

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `bugs/escaped/found:unknown (count)`: bottleneck

<!-- kixel.quest finding {"series":"bugs/escaped/found:unknown (count)","culprit":null} -->

- Series: [`bugs/escaped/found:unknown (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-23: 1 against a baseline of 0, +1. The largest share of escaped bugs, 62.71% of its measure over the 30 days to 2026-11-23; its last 7 days ran worse than the 30 before, median 1 over 7 days against 0 over 30, one-sided Mann-Whitney U, p = 0.0005299.
