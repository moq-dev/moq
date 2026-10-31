# [XS] Re-measure the browser budgets after the estimator capped one observation's weight

## Goal

Seven scheduled nightlies on `main` from 2026-10-29 on, after the estimator
began capping how much weight one observation takes, the player changes
that followed, and the relay history-fill fix, show the AAC `bursty` browser rows discarding
about as much audio a minute as the Opus `bursty` rows, and the Opus
`near-zero` row holding a 60 ms target with no underrun episodes. `test/audio-quality/budgets.json`
carries every browser row's ceilings re-measured from them.

## Plan

Waits on the nightlies: check the `audio-quality-run` artifacts of the
scheduled Nightly runs on `main` (`gh run download <id> -n audio-quality-run`)
once seven have run. Never dispatch the nightly by hand. The artifacts expire
after 7 days, so download each run's as it lands rather than waiting for the
seventh.

Decided 2026-10-30 (maintainer): the count restarts at the 2026-10-29
nightly, the first after
[moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168), because the
2026-10-25 to 10-28 nightlies measured the relay history-fill bug it fixed
(below). One has run (2026-10-29), with the seventh due about 2026-11-04.
This replaces the 2026-10-27 restart at the 2026-10-25 nightly, which
followed the post-cap estimator and player changes
([moq-dev/moq#6359](https://github.com/moq-dev/moq/pull/6359),
[moq-dev/moq#6525](https://github.com/moq-dev/moq/pull/6525),
[moq-dev/moq#6484](https://github.com/moq-dev/moq/pull/6484),
[moq-dev/moq#6609](https://github.com/moq-dev/moq/pull/6609)). No earlier
run counts toward the seven. Re-measure by the rule in the budgets note, and drop the note's sentence
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

The 2026-10-25 to 10-28 nightlies
([40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303),
[40332938121](https://github.com/moq-dev/moq/actions/runs/40332938121),
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673),
[40698329026](https://github.com/moq-dev/moq/actions/runs/40698329026))
measured a relay bug rather than the player: 2 to 4.5 s into every row except
`fixed-250` and Opus `near-zero`, the subscriber received 15 to 25 s of the
publisher's cached history, newest first. On the `wide` rows the estimator
took that as a late interval and held a 2020 ms target for about 20 s,
then discarded about 1.8 to 1.9 s of audio a minute falling back, every night;
the 10-28 Opus `bursty` 540 ms target is the same arrival. No `wide` or
`bursty` row logged more than one long task, of at most 58 ms.
[moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168) stopped a fresh
subscriber filling history below its start, merged 2026-10-29T00:52Z, and the
[2026-10-29 nightly](https://github.com/moq-dev/moq/actions/runs/40885347517)
is the first after it and the first that counts: the oldest frame any row
received is about 2.6 s behind live, every browser row is within budget, and
AAC `wide` discards 205.6 ms/min at a 323.2 ms target p95, Opus `wide` 243.4
at 460. On it AAC `bursty` discards 162.9 ms a minute against 0 for Opus
`bursty`, and Opus `near-zero` has no underrun episodes at a 60 ms target
p95. Across the four earlier nights Opus `near-zero` held 60 ms twice and
80 twice: the 120 ms prior decays to 60 only at 21.4 s, after the 5 s
warmup. Each night's browser metrics are kept
in [moq-dev/moq#7286](https://github.com/moq-dev/moq/pull/7286), since the
artifacts expire. The separate publisher fairness timeout was
excess per-object work, fixed by [moq-dev/moq#7133](https://github.com/moq-dev/moq/pull/7133).
