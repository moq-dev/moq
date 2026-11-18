# [M] Browser audio's ordered reader grades no worse than before #8799

## Goal

The Chromium `bursty` and `congestion` rows of `just test audio-quality`, with
browser audio reading its holes from `@moq/net`'s ordered reader, grade no
worse than before [moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799)
gave the audio consumer the player's clock, measured on the nightly runner,
and the one full starvation seen locally is explained or fixed.

## Plan

Decided 2026-11-16 (maintainer, /quest-run): ✅ the transport decides an
audio hole, through an ordered reader whose budget is the ring's size
(rejected: first measure the playhead rule against a hand-written wait in
the container consumer; keep the playhead rule and the separate wait).

[moq-dev/moq#9068](https://github.com/moq-dev/moq/pull/9068) moved browser
audio onto the ordered reader: `Ordered.nextGroup` returns a `Gap` for each
group it gives up on, audio reads through a second subscription whose
budget is what the ring holds plus the stretch bound and the warmup, and
the audio container consumer plays each `Gap` as its hole. Firefox's
`after resume` window stays audible.

Its local Chromium measurement, three runs each of the PR, its merge base,
and `cf23b5ae04~1` (before #8799), could not grade the rows: the machine sat
at a load average of 30 to 70, and render underruns reached 580 a minute on
the merge base. The player-side numbers were level with the merge base on
`congestion`, which also trimmed about half the audio it did before #8799.
On `bursty` they were mixed, and one Opus `bursty` run of seven starved for
its last 7.5 s: the playhead held still while the render clock ran, which no
merge-base or pre-#8799 run did, and four reruns did not repeat it.

Work:

- Grade the Chromium `bursty` and `congestion` rows from the nightly
  runner's `audio-quality-run` artifacts after #9068 lands, against the
  nights before it and before #8799.
- Find what starved that run. The audio container consumer
  (`js/hang/src/container/consumer.ts`) in ordered mode has nothing that
  passes an active group that opened but never delivers a frame until the
  max buffer fills, which #8799's playhead covered. A trace recorded with
  `--trace` from a run that starves would replay it.

## Related

- [moq-net's last pub items with a single container-reader caller go private](/quest/m2/unused-api/net.md) - the Rust container reader moving onto `Gap`, this quest's native counterpart
