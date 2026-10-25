# [XS] Re-measure the browser budgets after the estimator capped one observation's weight

## Goal

Seven scheduled nightlies on `main` after the estimator began capping how much
weight one observation takes show the AAC `bursty` browser rows discarding
about as much audio a minute as the Opus `bursty` rows, and the Opus
`near-zero` row holding a 60 ms target with no underrun episodes. `test/audio-quality/budgets.json`
carries every browser row's ceilings re-measured from them.

## Plan

Waits on the nightlies: check the `audio-quality-run` artifacts of the
scheduled Nightly runs on `main` (`gh run download <id> -n audio-quality-run`)
once seven have run since the change merged. Never dispatch the nightly by
hand. Re-measure by the rule in the budgets note, and drop the note's sentence
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
minute, 0 stalls, and 0 silence. This supplies one of the seven required
scheduled runs, not a reason to change the estimator or recalibrate early.

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
