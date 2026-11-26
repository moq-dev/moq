# [M] Investigate rs/moq-net/benches/announce.rs::benches::bench::announce health::churn lite06 10000 instructions, r…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::churn_lite06[10000] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::churn_lite06[10000] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::churn_lite06[10000] instructions`](https://kixel.quest/factory/series/1454), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 1451 instructions against a baseline of 1748 instructions, −297 instructions (−16.99%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::churn_lite06[1000] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::churn_lite06[1000] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::churn_lite06[1000] instructions`](https://kixel.quest/factory/series/1452), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 1456 instructions against a baseline of 1753 instructions, −297 instructions (−16.94%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::churn_lite06[100] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::churn_lite06[100] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::churn_lite06[100] instructions`](https://kixel.quest/factory/series/1450), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 1451 instructions against a baseline of 1748 instructions, −297 instructions (−16.99%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[10000x1000] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite06[10000x1000] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[10000x1000] instructions`](https://kixel.quest/factory/series/1462), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 31677900 instructions against a baseline of 34988900 instructions, −3311000 instructions (−9.463%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[1000x0] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite06[1000x0] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[1000x0] instructions`](https://kixel.quest/factory/series/1458), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 2871320 instructions against a baseline of 3172320 instructions, −301000 instructions (−9.488%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[1000x1000] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite06[1000x1000] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[1000x1000] instructions`](https://kixel.quest/factory/series/1460), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 6013770 instructions against a baseline of 6615770 instructions, −602000 instructions (−9.099%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[100x0] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite06[100x0] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite06[100x0] instructions`](https://kixel.quest/factory/series/1456), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 270917 instructions against a baseline of 301017 instructions, −30100 instructions (−9.999%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[10000x1000] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite07[10000x1000] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[10000x1000] instructions`](https://kixel.quest/factory/series/1463), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 42587600 instructions against a baseline of 46119200 instructions, −3531600 instructions (−7.658%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[1000x0] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite07[1000x0] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[1000x0] instructions`](https://kixel.quest/factory/series/1459), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 3887150 instructions against a baseline of 4208690 instructions, −321540 instructions (−7.64%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[1000x1000] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite07[1000x1000] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[1000x1000] instructions`](https://kixel.quest/factory/series/1461), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 8078860 instructions against a baseline of 8721390 instructions, −642530 instructions (−7.367%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.

### `rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[100x0] instructions`: regression since #9357

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/announce.rs::benches::bench::announce_health::decode_lite07[100x0] instructions","culprit":9357} -->

- Series: [`rs/moq-net/benches/announce.rs::benches::bench::announce_health::decode_lite07[100x0] instructions`](https://kixel.quest/factory/series/1457), in instructions.
- Culprit: [#9357](https://github.com/moq-dev/moq/pull/9357), feat(net)!: lite-07 Path Keep counts decoded parts. [Its diff](https://github.com/moq-dev/moq/pull/9357/files) changed 7 paths: `js/net/src/lite/announce.test.ts`, `js/net/src/lite/announce.ts`, `quest/m1/lite07-routing/README.md`, `quest/m2/README.md`, `quest/m2/lite07-finalize.md`, `quest/m2/lite07-path-keep-parts.md`, `rs/moq-net/src/lite/compress.rs`.
- Session `e36615d0-3897-4039-ac30-ebdc4f0442c5` on it: claude, iterating, claude-opus-5-5 high, 44 minutes.
- 2026-11-26: 367170 instructions against a baseline of 399806 instructions, −32636 instructions (−8.163%), at [`37ebe62`](https://github.com/moq-dev/moq/commit/37ebe6230c9546acb76ea1fac82374855722e2fb). Student's t prediction test of 34e760d against the 8 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg); bisected over 2 commits to 37ebe62.
