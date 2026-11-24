# [M] Investigate conflicts/events (count), cost/by-model/model:claude-opus-5-5 (USD), and 5 more

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `conflicts/events (count)`: bottleneck

<!-- kixel.quest finding {"series":"conflicts/events (count)","culprit":null} -->

- Series: [`conflicts/events (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-24: 0 against a baseline of 0, +0. The largest share of conflicts, 10.14% of PRs over the 30 days to 2026-11-24; too few measurements for a trend.

### `cost/by-model/model:claude-opus-5-5 (USD)`: bottleneck

<!-- kixel.quest finding {"series":"cost/by\u002dmodel/model:claude\u002dopus\u002d5\u002d5 (USD)","culprit":null} -->

- Series: [`cost/by-model/model:claude-opus-5-5 (USD)`](https://kixel.quest/factory#bottlenecks-508763694), in USD.
- 2026-11-24: 4.141 USD against a baseline of 4.141 USD, +0 USD (+0%). The largest share of cost, 100% of its measure over the 30 days to 2026-11-24; too few measurements for a trend.

### `friction/split/category:rework (tokens)`: bottleneck

<!-- kixel.quest finding {"series":"friction/split/category:rework (tokens)","culprit":null} -->

- Series: [`friction/split/category:rework (tokens)`](https://kixel.quest/factory#bottlenecks-508763694), in tokens.
- 2026-11-24: 0 tokens against a baseline of 0 tokens, +0 tokens. The largest share of friction, 9.711% of its measure over the 30 days to 2026-11-24; too few measurements for a trend.

### `host/gate-wait/command:cargo nextest (h)`: bottleneck

<!-- kixel.quest finding {"series":"host/gate\u002dwait/command:cargo nextest (h)","culprit":null} -->

- Series: [`host/gate-wait/command:cargo nextest (h)`](https://kixel.quest/factory#bottlenecks-508763694), in h.
- 2026-11-24: 0 h against a baseline of 0 h, +0 h. The largest share of host time, 44.21% of its measure over the 30 days to 2026-11-24; too few measurements for a trend.

### `reviews/empty/reviewer:Codex, gpt-6.1-sol, medium effort (count)`: bottleneck

<!-- kixel.quest finding {"series":"reviews/empty/reviewer:Codex, gpt\u002d6.1\u002dsol, medium effort (count)","culprit":null} -->

- Series: [`reviews/empty/reviewer:Codex, gpt-6.1-sol, medium effort (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-24: 1 against a baseline of 1, +0 (+0%). The largest share of reviews, 75% of its measure over the 30 days to 2026-11-24; too few measurements for a trend.

### `rework/fixes (count)`: bottleneck

<!-- kixel.quest finding {"series":"rework/fixes (count)","culprit":null} -->

- Series: [`rework/fixes (count)`](https://kixel.quest/factory#bottlenecks-508763694).
- 2026-11-24: 0 against a baseline of 0, +0. The largest share of rework, 2.899% of PRs over the 30 days to 2026-11-24; too few measurements for a trend.

### `ship/pick (h)`: bottleneck

<!-- kixel.quest finding {"series":"ship/pick (h)","culprit":null} -->

- Series: [`ship/pick (h)`](https://kixel.quest/factory#bottlenecks-508763694), in h.
- 2026-11-24: 0.02222 h against a baseline of 0.02222 h, +0 h (+0%). The largest share of time to ship, 29.47% of its phases over the 30 days to 2026-11-24; too few measurements for a trend.
