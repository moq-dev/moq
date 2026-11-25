# [M] Investigate rs/moq-net/benches/stats.rs::benches::session report::stats/session-report 1000x1/connections=1 ins…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x1/connections=1] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1000x1/connections=1] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x1/connections=1] instructions`](https://kixel.quest/factory/series/1554), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 644939 instructions against a baseline of 613408 instructions, +31531 instructions (+5.14%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x1/connections=4] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1000x1/connections=4] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x1/connections=4] instructions`](https://kixel.quest/factory/series/1555), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 1243540 instructions against a baseline of 1105410 instructions, +138130 instructions (+12.5%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x10/connections=1] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1000x10/connections=1] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x10/connections=1] instructions`](https://kixel.quest/factory/series/1557), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 2724990 instructions against a baseline of 2437410 instructions, +287580 instructions (+11.8%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x10/connections=4] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1000x10/connections=4] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1000x10/connections=4] instructions`](https://kixel.quest/factory/series/1558), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 8587040 instructions against a baseline of 7357410 instructions, +1229630 instructions (+16.71%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x1/connections=1] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[100x1/connections=1] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x1/connections=1] instructions`](https://kixel.quest/factory/series/1548), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 65684 instructions against a baseline of 60436 instructions, +5248 instructions (+8.684%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x1/connections=4] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[100x1/connections=4] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x1/connections=4] instructions`](https://kixel.quest/factory/series/1549), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 123679 instructions against a baseline of 109636 instructions, +14043 instructions (+12.81%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x10/connections=1] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[100x10/connections=1] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x10/connections=1] instructions`](https://kixel.quest/factory/series/1551), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 274228 instructions against a baseline of 244036 instructions, +30192 instructions (+12.37%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x10/connections=4] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[100x10/connections=4] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[100x10/connections=4] instructions`](https://kixel.quest/factory/series/1552), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 865086 instructions against a baseline of 736036 instructions, +129050 instructions (+17.53%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x1/connections=1] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1x1/connections=1] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x1/connections=1] instructions`](https://kixel.quest/factory/series/1542), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 1149 instructions against a baseline of 1001 instructions, +148 instructions (+14.79%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x1/connections=4] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1x1/connections=4] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x1/connections=4] instructions`](https://kixel.quest/factory/series/1543), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 1723 instructions against a baseline of 1493 instructions, +230 instructions (+15.41%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x10/connections=1] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1x10/connections=1] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x10/connections=1] instructions`](https://kixel.quest/factory/series/1545), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 4203 instructions against a baseline of 2837 instructions, +1366 instructions (+48.15%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).

### `rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x10/connections=4] instructions`: regression since #9481

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::session_report::stats/session\u002dreport[1x10/connections=4] instructions","culprit":9481} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::session_report::stats/session-report[1x10/connections=4] instructions`](https://kixel.quest/factory/series/1546), in instructions.
- Culprit: [#9481](https://github.com/moq-dev/moq/pull/9481), feat(net): a wire source that stats read on each report. [Its diff](https://github.com/moq-dev/moq/pull/9481/files) changed 4 paths: `rs/moq-net/src/stats.rs`, `rs/moq-net/src/transport.rs`, `rs/qmux/src/socket.rs`, `rs/qmux/src/tcp.rs`.
- 2026-11-25: 11135 instructions against a baseline of 7757 instructions, +3378 instructions (+43.55%), at [`5404f8e`](https://github.com/moq-dev/moq/commit/5404f8e78d617993eee1a71622e26e4dda0e4f7d). Student's t prediction test of 5404f8e against the 5 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 835 series the run judged (Benjamini-Hochberg).
