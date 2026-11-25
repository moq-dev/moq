# [M] Investigate rs/moq-net/benches/frame.rs::benches::bench::frame read grow/size=16384/publishers=1/readers=128/ar…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=32] instructions`: regression since #9546

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=32] instructions","culprit":9546} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1643), in instructions.
- Culprit: [#9546](https://github.com/moq-dev/moq/pull/9546), fix(net): a frame read through a front resumes on a flight once the pool evicts its copy. [Its diff](https://github.com/moq-dev/moq/pull/9546/files) changed 3 paths: `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/model/frame.rs`, `rs/moq-net/src/model/resume.rs`.
- Session `aa2f7ebd59cc061c1` on it: claude, iterating, claude-opus-5-5 high, 3 minutes.
- Session `a8fa0bf926e06deaf` on it: claude, implementing, claude-opus-5-5 high, 47 minutes.
- Session `af44e0bf2eea84a09` on it: claude, implementing, claude-opus-5-5 high, 88 minutes.
- Session `a09bbeefb72edd6bf` on it: claude, reviewing, claude-sonnet-5-5 high, 94 minutes.
- Session `aa8400c4aa4354f87` on it: claude, iterating, claude-opus-5-5 high, 53 minutes.
- Session `a21c5cf7776395942` on it: claude, iterating, claude-opus-5-5 high, 59 minutes.
- 2026-11-25: 1979530 instructions against a baseline of 2084000 instructions, −104800 instructions (−5.03%), at [`3717c89`](https://github.com/moq-dev/moq/commit/3717c893a96c92ecae666115757972773be2ac3b). Student's t prediction test of 3717c89 against the 11 commits before it at its level, p = 1.4e-22, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=32] instructions`: regression since #9546

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=32] instructions","culprit":9546} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1649), in instructions.
- Culprit: [#9546](https://github.com/moq-dev/moq/pull/9546), fix(net): a frame read through a front resumes on a flight once the pool evicts its copy. [Its diff](https://github.com/moq-dev/moq/pull/9546/files) changed 3 paths: `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/model/frame.rs`, `rs/moq-net/src/model/resume.rs`.
- Session `aa2f7ebd59cc061c1` on it: claude, iterating, claude-opus-5-5 high, 3 minutes.
- Session `a8fa0bf926e06deaf` on it: claude, implementing, claude-opus-5-5 high, 47 minutes.
- Session `af44e0bf2eea84a09` on it: claude, implementing, claude-opus-5-5 high, 88 minutes.
- Session `a09bbeefb72edd6bf` on it: claude, reviewing, claude-sonnet-5-5 high, 94 minutes.
- Session `aa8400c4aa4354f87` on it: claude, iterating, claude-opus-5-5 high, 53 minutes.
- Session `a21c5cf7776395942` on it: claude, iterating, claude-opus-5-5 high, 59 minutes.
- 2026-11-25: 7916450 instructions against a baseline of 8335000 instructions, −419000 instructions (−5.027%), at [`3717c89`](https://github.com/moq-dev/moq/commit/3717c893a96c92ecae666115757972773be2ac3b). Student's t prediction test of 3717c89 against the 11 commits before it at its level, p = 1.1e-24, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=32] instructions`: regression since #9546

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=32] instructions","culprit":9546} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1631), in instructions.
- Culprit: [#9546](https://github.com/moq-dev/moq/pull/9546), fix(net): a frame read through a front resumes on a flight once the pool evicts its copy. [Its diff](https://github.com/moq-dev/moq/pull/9546/files) changed 3 paths: `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/model/frame.rs`, `rs/moq-net/src/model/resume.rs`.
- Session `aa2f7ebd59cc061c1` on it: claude, iterating, claude-opus-5-5 high, 3 minutes.
- Session `a8fa0bf926e06deaf` on it: claude, implementing, claude-opus-5-5 high, 47 minutes.
- Session `af44e0bf2eea84a09` on it: claude, implementing, claude-opus-5-5 high, 88 minutes.
- Session `a09bbeefb72edd6bf` on it: claude, reviewing, claude-sonnet-5-5 high, 94 minutes.
- Session `aa8400c4aa4354f87` on it: claude, iterating, claude-opus-5-5 high, 53 minutes.
- Session `a21c5cf7776395942` on it: claude, iterating, claude-opus-5-5 high, 59 minutes.
- 2026-11-25: 1971820 instructions against a baseline of 2077000 instructions, −104700 instructions (−5.042%), at [`3717c89`](https://github.com/moq-dev/moq/commit/3717c893a96c92ecae666115757972773be2ac3b). Student's t prediction test of 3717c89 against the 11 commits before it at its level, p = 4.4e-25, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=32] instructions`: regression since #9546

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=32] instructions","culprit":9546} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1637), in instructions.
- Culprit: [#9546](https://github.com/moq-dev/moq/pull/9546), fix(net): a frame read through a front resumes on a flight once the pool evicts its copy. [Its diff](https://github.com/moq-dev/moq/pull/9546/files) changed 3 paths: `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/model/frame.rs`, `rs/moq-net/src/model/resume.rs`.
- Session `aa2f7ebd59cc061c1` on it: claude, iterating, claude-opus-5-5 high, 3 minutes.
- Session `a8fa0bf926e06deaf` on it: claude, implementing, claude-opus-5-5 high, 47 minutes.
- Session `af44e0bf2eea84a09` on it: claude, implementing, claude-opus-5-5 high, 88 minutes.
- Session `a09bbeefb72edd6bf` on it: claude, reviewing, claude-sonnet-5-5 high, 94 minutes.
- Session `aa8400c4aa4354f87` on it: claude, iterating, claude-opus-5-5 high, 53 minutes.
- Session `a21c5cf7776395942` on it: claude, iterating, claude-opus-5-5 high, 59 minutes.
- 2026-11-25: 7886830 instructions against a baseline of 8306000 instructions, −418900 instructions (−5.043%), at [`3717c89`](https://github.com/moq-dev/moq/commit/3717c893a96c92ecae666115757972773be2ac3b). Student's t prediction test of 3717c89 against the 11 commits before it at its level, p = 4.1e-25, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).
