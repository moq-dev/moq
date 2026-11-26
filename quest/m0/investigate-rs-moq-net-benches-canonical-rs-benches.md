# [M] Investigate rs/moq-net/benches/canonical.rs::benches::canonical::canonical storage::cleanup 1x1 instructions, r…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-net/benches/canonical.rs::benches::canonical::canonical_storage::cleanup[1x1] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/canonical.rs::benches::canonical::canonical_storage::cleanup[1x1] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/canonical.rs::benches::canonical::canonical_storage::cleanup[1x1] instructions`](https://kixel.quest/factory/series/1390), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 2494 instructions against a baseline of 2277 instructions, +217 instructions (+9.53%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/canonical.rs::benches::canonical::canonical_storage::cleanup[1x8] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/canonical.rs::benches::canonical::canonical_storage::cleanup[1x8] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/canonical.rs::benches::canonical::canonical_storage::cleanup[1x8] instructions`](https://kixel.quest/factory/series/1392), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 3075 instructions against a baseline of 2858 instructions, +217 instructions (+7.593%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1643), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 2084600 instructions against a baseline of 1729000 instructions, +355500 instructions (+20.56%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 7.0e-46, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=128/arrivals=4] instructions`](https://kixel.quest/factory/series/1642), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 393659 instructions against a baseline of 349900 instructions, +43770 instructions (+12.51%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 2.2e-36, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=16/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=16/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=16/arrivals=32] instructions`](https://kixel.quest/factory/series/1641), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 310379 instructions against a baseline of 265500 instructions, +44920 instructions (+16.92%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 1.7e-36, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=16/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=16/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=1/readers=16/arrivals=4] instructions`](https://kixel.quest/factory/series/1640), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 66650 instructions against a baseline of 60510 instructions, +6139 instructions (+10.15%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 5.4e-27, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=1/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=1/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=1/arrivals=32] instructions`](https://kixel.quest/factory/series/1645), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 279093 instructions against a baseline of 264900 instructions, +14210 instructions (+5.364%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 5.3e-31, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1649), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 8335750 instructions against a baseline of 6914000 instructions, +1422000 instructions (+20.56%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 1.9e-54, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=128/arrivals=4] instructions`](https://kixel.quest/factory/series/1648), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 1562970 instructions against a baseline of 1391000 instructions, +172400 instructions (+12.4%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 2.0e-42, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=16/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=16/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=16/arrivals=32] instructions`](https://kixel.quest/factory/series/1647), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 1231030 instructions against a baseline of 1052000 instructions, +179100 instructions (+17.03%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 1.5e-44, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=16/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=16/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=16384/publishers=4/readers=16/arrivals=4] instructions`](https://kixel.quest/factory/series/1646), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 255637 instructions against a baseline of 234100 instructions, +21540 instructions (+9.199%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 5.4e-33, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=2097152/publishers=1/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=2097152/publishers=1/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=2097152/publishers=1/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1667), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 6101260 instructions against a baseline of 5744000 instructions, +356900 instructions (+6.213%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 6.7e-46, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=2097152/publishers=4/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[grow/size=2097152/publishers=4/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[grow/size=2097152/publishers=4/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1673), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 24406400 instructions against a baseline of 22990000 instructions, +1419000 instructions (+6.174%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 9.6e-47, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1631), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 2076510 instructions against a baseline of 1721000 instructions, +355300 instructions (+20.64%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 8.1e-48, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=128/arrivals=4] instructions`](https://kixel.quest/factory/series/1630), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 390508 instructions against a baseline of 346600 instructions, +43920 instructions (+12.67%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 2.1e-36, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=16/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=16/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=16/arrivals=32] instructions`](https://kixel.quest/factory/series/1629), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 303343 instructions against a baseline of 258400 instructions, +44910 instructions (+17.38%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 1.7e-36, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=16/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=16/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=1/readers=16/arrivals=4] instructions`](https://kixel.quest/factory/series/1628), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 63575 instructions against a baseline of 57550 instructions, +6026 instructions (+10.47%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 6.6e-27, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1637), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 8305840 instructions against a baseline of 6886000 instructions, +1420000 instructions (+20.62%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 1.7e-52, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=128/arrivals=4] instructions`](https://kixel.quest/factory/series/1636), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 1548550 instructions against a baseline of 1375000 instructions, +173900 instructions (+12.65%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 2.1e-44, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=16/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=16/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=16/arrivals=32] instructions`](https://kixel.quest/factory/series/1635), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 1200100 instructions against a baseline of 1022000 instructions, +178400 instructions (+17.47%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 1.4e-42, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=16/arrivals=4] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=16/arrivals=4] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=16384/publishers=4/readers=16/arrivals=4] instructions`](https://kixel.quest/factory/series/1634), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 241312 instructions against a baseline of 218100 instructions, +23190 instructions (+10.63%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 2.4e-33, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=2097152/publishers=1/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=2097152/publishers=1/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=2097152/publishers=1/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1655), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 4127790 instructions against a baseline of 3774000 instructions, +353900 instructions (+9.377%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 7.4e-46, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=2097152/publishers=4/readers=128/arrivals=32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/frame.rs::benches::bench::frame_read[upfront/size=2097152/publishers=4/readers=128/arrivals=32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/frame.rs::benches::bench::frame_read[upfront/size=2097152/publishers=4/readers=128/arrivals=32] instructions`](https://kixel.quest/factory/series/1661), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 16506000 instructions against a baseline of 15084200 instructions, +1421800 instructions (+9.426%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x10/connections=1] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1x10/connections=1] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x10/connections=1] instructions`](https://kixel.quest/factory/series/1545), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 4485 instructions against a baseline of 4203 instructions, +282 instructions (+6.709%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 7 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x1024] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x1024] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x1024] instructions`](https://kixel.quest/factory/series/1504), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x1] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x1] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x1] instructions`](https://kixel.quest/factory/series/1502), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1024x32] instructions`](https://kixel.quest/factory/series/1503), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x1024] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x1024] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x1024] instructions`](https://kixel.quest/factory/series/1498), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x1] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x1] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x1] instructions`](https://kixel.quest/factory/series/1496), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[1x32] instructions`](https://kixel.quest/factory/series/1497), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x1024] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x1024] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x1024] instructions`](https://kixel.quest/factory/series/1501), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x1] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x1] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x1] instructions`](https://kixel.quest/factory/series/1499), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.

### `rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x32] instructions`: regression since #9422

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x32] instructions","culprit":9422} -->

- Series: [`rs/moq-net/benches/tail.rs::benches::bench_pending_tail::track_pending_tail::repoll[32x32] instructions`](https://kixel.quest/factory/series/1500), in instructions.
- Culprit: [#9422](https://github.com/moq-dev/moq/pull/9422), fix(net): a pool held over capacity by its live edges still serves them. [Its diff](https://github.com/moq-dev/moq/pull/9422/files) changed 23 paths: `doc/bin/relay/config.md`, `js/net/src/fill.test.ts`, `js/net/src/lite/fills.test.ts`, `js/net/src/lite/fills.ts`, `js/net/src/track.ts`, `quest/m1/ordered-late-arrival-old.md`, `quest/m1/pool-pinned-debt.md`, `quest/m1/relay-evicted-reset.md`, `rs/moq-net/src/lite/publisher.rs`, `rs/moq-net/src/lite/subscriber.rs`, and 13 more.
- Session `144bba8a-3535-41b2-a1d4-f105a156fe22` on it: claude, iterating, claude-opus-5-5 high, 953 minutes.
- 2026-11-26: 231 instructions against a baseline of 154 instructions, +77 instructions (+50%), at [`35e6841`](https://github.com/moq-dev/moq/commit/35e6841f4fdd52d025490d7e8a2d0af40609c7f2). Student's t prediction test of 35e6841 against the 12 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 6 commits to 35e6841.
