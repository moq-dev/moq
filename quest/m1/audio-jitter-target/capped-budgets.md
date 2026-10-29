# [XS] Re-measure the browser budgets after the estimator capped one observation's weight

## Goal

Seven scheduled nightlies on `main` from 2026-10-25 on, after the estimator
began capping how much weight one observation takes and the player changes
that followed, show the AAC `bursty` browser rows discarding
about as much audio a minute as the Opus `bursty` rows, and the Opus
`near-zero` row holding a 60 ms target with no underrun episodes. `test/audio-quality/budgets.json`
carries every browser row's ceilings re-measured from them.

## Plan

Waits on the nightlies: check the `audio-quality-run` artifacts of the
scheduled Nightly runs on `main` (`gh run download <id> -n audio-quality-run`)
once seven have run. Never dispatch the nightly by hand. The artifacts expire
after 7 days, so download each run's as it lands rather than waiting for the
seventh.

Decided 2026-10-27 (maintainer): the count restarts at the 2026-10-25
nightly, because estimator and player changes landed after the cap
([moq-dev/moq#6359](https://github.com/moq-dev/moq/pull/6359),
[moq-dev/moq#6525](https://github.com/moq-dev/moq/pull/6525),
[moq-dev/moq#6484](https://github.com/moq-dev/moq/pull/6484),
[moq-dev/moq#6609](https://github.com/moq-dev/moq/pull/6609)), the last on
2026-10-24. That makes 3 of 7 so far (2026-10-25, 10-26 and 10-27), with the
seventh due about 2026-10-31. The earlier post-cap runs below no longer
count toward the seven. Re-measure by the rule in the budgets note, and drop the note's sentence
that budgets the AAC bursty discards against Opus's as measured. Nightlies
from before the one-ring removal also carry `-isolated` rows; grade only the
`-plain` rows, which are the browser rows now.

Decided 2026-10-22 (maintainer): the Opus near-zero budgets quest folded in
here, so one pass over the same seven nightlies covers both. The cap
([#6164](https://github.com/moq-dev/moq/pull/6164)) merged 2026-10-22T00:10Z,
after the build flush, so post-cap nightlies are also post-flush.

The first post-cap scheduled run, [2026-10-22 nightly](https://github.com/moq-dev/moq/actions/runs/39779729461),
failed only the browser Opus `wide` target ceilings: `isolated` p95 was
380 ms against 270 ms, and `plain` was 400 ms against 300 ms. In the
`audio-quality-run` artifact, both `chromium-opus-48000-wide-*.ndjson`
series hold their startup target until about 21 s, then settle at 200 to
220 ms; both matching `.longtasks.json` arrays are empty. The isolated
row has zero underruns, stalls, and silence; plain has 2.2 underruns a
minute, 0 stalls, and 0 silence. It predates the restarted count, and is
not a reason to change the estimator or recalibrate early.

The near-zero stall was never the isolated ring. That row is simply the
matrix's first, and a cold `cargo build` left about 800 MB dirty when it
started. On the ARM runner each periodic writeback burst froze the page's main
thread for 100 to 320 ms until the backlog drained, and the harness now
flushes its build before the first row. If the first row still shows
long-task clusters, compare `<tag>.longtasks.json` against the runner's disk
activity before changing the player.

The cap changed every row's startup, not only AAC bursty's: the prior is
replaced in about five seconds instead of two, and a late interval in those
first seconds holds the target for up to one 20 s half-life instead of falling
back within seconds. Replays of local captures held the clean rows' targets
where they were, but one live Opus `near-zero` `plain` run held a startup
hiccup at 160 ms for 20 s, over that row's 95 ms ceiling. If a nightly breaches
a startup target ceiling that way, budget it as measured rather than reopening
the estimator.

On replays of the 2026-10-18 nightly capture and of two local captures, the
cap cut AAC bursty discards from 540 to 860 ms a minute to 0 to 170, and its
target p95 from 663 ms to 343 to 363. If the nightlies still show AAC bursty
discarding several times Opus's, the remaining source is the publisher: each
seven-frame PES flush is seven one-frame groups, delivered newest first, so the
oldest frame of a flush waits behind every newer group on a path that cannot
carry them together.

The fourth run, [2026-10-28 nightly](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001011),
breaches six browser ceilings: AAC wide discards 1790.9 ms/min against 460
and target p95 2023.2 ms against 450; Opus bursty target p95 540 ms against
370, mild 160 against 120, and wide discards 1913.9 ms/min against 420 and
target p95 2020 ms against 570. Preserve this run with the other post-cap
measurements and distinguish startup/runner stalls from sustained estimator
behavior before re-measuring. The separate publisher fairness timeout is
tracked by [Nightly](/quest/m0/nightly/publisher-fairness.md).
