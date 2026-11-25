# [M] Investigate bugs/escaped/found:reviewer (count), friction/iterates/iterate:idle (count), and 3 more

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `bugs/escaped/found:reviewer (count)`: bottleneck

<!-- kixel.quest finding {"series":"bugs/escaped/found:reviewer (count)","culprit":null} -->

- Series: [`bugs/escaped/found:reviewer (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-25: 1 against a baseline of 0, +1. Worsening: its last 7 days ran worse than the 30 before, median 1 over 7 days against 0 over 30, one-sided Mann-Whitney U, p = 0.0005299, under 0.01 over the 5 series tested; 4.505% of its measure.

### `friction/iterates/iterate:idle (count)`: bottleneck

<!-- kixel.quest finding {"series":"friction/iterates/iterate:idle (count)","culprit":null} -->

- Series: [`friction/iterates/iterate:idle (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-25: 0 against a baseline of 0, +0. The largest share of friction, 22.22% of its measure over the 30 days to 2026-11-25; too few measurements for a trend.

### `host/gate-cpu/command:just check (h)`: bottleneck

<!-- kixel.quest finding {"series":"host/gate\u002dcpu/command:just check (h)","culprit":null} -->

- Series: [`host/gate-cpu/command:just check (h)`](https://kixel.quest/factory#bottlenecks-508763694), in h.
- 2026-11-25: 0.1165 h against a baseline of 0.1165 h, +0 h (+0%). The largest share of host time, 36.25% of its measure over the 30 days to 2026-11-25; too few measurements for a trend.

### `reviews/refuted/reviewer:Claude Code, claude-sonnet-5-5, medium effort, second (count)`: bottleneck

<!-- kixel.quest finding {"series":"reviews/refuted/reviewer:Claude Code, claude\u002dsonnet\u002d5\u002d5, medium effort, second (count)","culprit":null} -->

- Series: [`reviews/refuted/reviewer:Claude Code, claude-sonnet-5-5, medium effort, second (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-25: 2.5 against a baseline of 2.5, +0 (+0%). The largest share of reviews, 76% of its measure over the 30 days to 2026-11-25; too few measurements for a trend.

### `ship/review (h)`: bottleneck

<!-- kixel.quest finding {"series":"ship/review (h)","culprit":null} -->

- Series: [`ship/review (h)`](https://kixel.quest/factory#bottlenecks-508763694), in h.
- 2026-11-25: 0.02139 h against a baseline of 0.02139 h, +0 h (+0%). The largest share of time to ship, 28.63% of its phases over the 30 days to 2026-11-25; too few measurements for a trend.
