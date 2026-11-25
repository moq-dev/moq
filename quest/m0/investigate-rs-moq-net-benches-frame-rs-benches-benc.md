# [M] Investigate rs/moq-net/benches/frame.rs::benches::bench::frame read upfront/size=16384/publishers=1/readers=1/a…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=1/arrivals=32] instructions`: drift

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=1/arrivals=32] instructions","culprit":null} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=1/arrivals=32] instructions`](https://kixel.quest/factory/series/1627), in instructions.
- 2026-11-25: 65880 instructions against a baseline of 62730 instructions, +3155 instructions (+5.029%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Welch's t-test of the 5 commits from 35e6841 against the 12 before them at its level, p = 9.3e-7 corrected over every split, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=1/arrivals=32] instructions`: drift

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=1/arrivals=32] instructions","culprit":null} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=1/arrivals=32] instructions`](https://kixel.quest/factory/series/1633), in instructions.
- 2026-11-25: 249600 instructions against a baseline of 237600 instructions, +11990 instructions (+5.047%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Welch's t-test of the 5 commits from 35e6841 against the 12 before them at its level, p = 0.000024 corrected over every split, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).
