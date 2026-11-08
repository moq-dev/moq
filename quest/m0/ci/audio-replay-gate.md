# [XS] The audio replay rows gate every merge

## Goal

`just test audio-quality --replays --enforce` runs in `just ci`, so a PR that
moves a replay row fails the merge gate unless it updates `budgets.json` in the
same PR.

## Plan

Decided 2026-11-07 (maintainer, /quest-complete on
[moq-dev/moq#8218](https://github.com/moq-dev/moq/pull/8218)): ✅ the replays
join `just ci`. [moq-dev/moq#8071](https://github.com/moq-dev/moq/pull/8071)
broke two replay rows on `main` with no PR check failing. The replays run only
from `.github/workflows/audio-quality.yml`, which nightly and release PRs call,
so the breakage surfaced only once a later PR ran them by hand.

The replays are deterministic and take seconds: no browser, relay, or shaper,
only Bun and the player's `js/` packages. Register them as a module in
`sh/dispatch.sh`'s impact map. Select it on whatever the replay plays through
(`js/hang/`, `js/watch/`, `js/net/`, `js/signals/`, `js/clock/`) and on the
harness itself (`test/audio-quality/`), so the queue's `just ci` runs it too.
Then decide whether `audio-quality.yml`'s release-PR job still earns its
runner, and update the audio-quality README's account of where the replays run.

`grade.ts --enforce` fails only a value over its ceiling, so a replay row that
improves still passes and leaves headroom for a later regression, against
`budgets.json`'s rule that a replay ceiling is exactly what it measures. Make
`--enforce` fail a replay row whose value differs from its budget in either
direction, keeping the ceiling comparison for the browser and native rows.
