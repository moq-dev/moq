# [M] Investigate rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection 128x32 instructions, r…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[128x32] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/retirement.rs::benches::retirement::retirement::selection[128x32] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[128x32] instructions`](https://kixel.quest/factory/series/1388), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 169193 instructions against a baseline of 153200 instructions, +16000 instructions (+10.44%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 1.0e-48, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.

### `rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[128x8] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/retirement.rs::benches::retirement::retirement::selection[128x8] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[128x8] instructions`](https://kixel.quest/factory/series/1387), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 55517 instructions against a baseline of 51610 instructions, +3907 instructions (+7.569%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 7.8e-18, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.

### `rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[32x32] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/retirement.rs::benches::retirement::retirement::selection[32x32] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[32x32] instructions`](https://kixel.quest/factory/series/1385), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 169192 instructions against a baseline of 153200 instructions, +16040 instructions (+10.47%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 4.1e-49, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.

### `rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[32x8] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/retirement.rs::benches::retirement::retirement::selection[32x8] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[32x8] instructions`](https://kixel.quest/factory/series/1384), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 56279 instructions against a baseline of 51830 instructions, +4445 instructions (+8.574%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 2.5e-10, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.

### `rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[8x32] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/retirement.rs::benches::retirement::retirement::selection[8x32] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[8x32] instructions`](https://kixel.quest/factory/series/1382), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 169078 instructions against a baseline of 153100 instructions, +16010 instructions (+10.46%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 1.1e-52, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.

### `rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[8x8] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/retirement.rs::benches::retirement::retirement::selection[8x8] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/retirement.rs::benches::retirement::retirement::selection[8x8] instructions`](https://kixel.quest/factory/series/1381), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 55793 instructions against a baseline of 51642 instructions, +4151 instructions (+8.038%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 5.0e-23, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.

### `rs/moq-net/benches/stats.rs::benches::request::stats/request[1x10] instructions`: regression since #9371

<!-- kixel.quest finding {"series":"rs/moq\u002dnet/benches/stats.rs::benches::request::stats/request[1x10] instructions","culprit":9371} -->

- Series: [`rs/moq-net/benches/stats.rs::benches::request::stats/request[1x10] instructions`](https://kixel.quest/factory/series/1534), in instructions.
- Culprit: [#9371](https://github.com/moq-dev/moq/pull/9371), fix(net): the origin ranks routes as the router does, and bridged routes keep drain and priority. [Its diff](https://github.com/moq-dev/moq/pull/9371/files) changed 45 paths: `doc/bin/relay/cluster.md`, `doc/concept/identity.md`, `drafts/draft-lcurley-moq-lite.md`, `js/justfile`, `js/net/bench/routed-claims.ts`, `js/net/src/claim.test-util.ts`, `js/net/src/consume.test.ts`, `js/net/src/consume.ts`, `js/net/src/hop.ts`, `js/net/src/ietf/publisher.ts`, and 35 more.
- Session `1fde88c7-1462-4d73-be7c-49057069cc94` on it: claude, iterating, claude-opus-5-5 high, 124 minutes.
- 2026-11-26: 1995 instructions against a baseline of 2109 instructions, −114 instructions (−5.405%), at [`45621b7`](https://github.com/moq-dev/moq/commit/45621b7b92e237ba3d7c60f0a48fa89a0875e731). Student's t prediction test of 2311175 against the 24 commits before it at its level, p = 0.0, held to a 1% false discovery rate over the 831 series the run judged (Benjamini-Hochberg); bisected over 4 commits to 45621b7.
