# [M] Investigate rs/moq-e2ee/benches/protect.rs::benches::bench protect::protect::grouped 1k instructions

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-e2ee/benches/protect.rs::benches::bench_protect::protect::grouped_1k instructions`: drift

<!-- kixel.quest finding {"series":"rs/moq\u002de2ee/benches/protect.rs::benches::bench_protect::protect::grouped_1k instructions","culprit":null} -->

- Series: [`rs/moq-e2ee/benches/protect.rs::benches::bench_protect::protect::grouped_1k instructions`](https://kixel.quest/factory/series/1819), in instructions.
- 2026-11-25: 13290 instructions against a baseline of 12629 instructions, +662.9 instructions (+5.249%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Welch's t-test of the 7 commits from 35e6841 against the 12 before them at its level, p = 0.0000036 corrected over every split, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).
