# [M] The bursty audio rows pass their budgets again

## Goal

Nightly's `Audio quality` (`just test audio-quality --runtimes chromium --enforce`) passes the
`chromium-opus-48000-bursty` and `chromium-aac-44100-bursty` rows on a scheduled run on
`main`, fixed at the cause, without loosening a budget.

## Plan

Found by the Nightly triage of 2026-11-26. Both rows passed in run
[45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582) (`5d86f05321`), then:

| row, metric | 11-24 | 11-25 ([45667362304](https://github.com/moq-dev/moq/actions/runs/45667362304), `420b8dcb0d`) | 11-26 ([45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463), `4fba5df73f`) | budget |
|---|---|---|---|---|
| opus bursty underruns/min | 35.9 | 0 | 1306.3 | 250 |
| opus bursty trimmed ms/min | 0 | 347.4 | 3870.5 | 280 |
| opus bursty target p95 ms | 262 | 542 | - | 400 |
| opus bursty silence share | 0.004 | 0 | 0.063 | 0.015 |
| aac bursty accelerated ms/min | 7.5 | 498.4 | - | - |
| aac bursty A/V lag p50 ms | -2.4 | 8.2 | - | 5 |

Two steps, so maybe two causes. Candidates for 11-25: audio and watch changes
[moq-dev/moq#9549](https://github.com/moq-dev/moq/pull/9549) (a hold keeps the stretch's
pace), [moq-dev/moq#9572](https://github.com/moq-dev/moq/pull/9572) (retained audio replays
only for a live subscription), [moq-dev/moq#9562](https://github.com/moq-dev/moq/pull/9562)
and [moq-dev/moq#9578](https://github.com/moq-dev/moq/pull/9578) (Opus concealment). For
11-26: the net changes between `420b8dcb0d` and `4fba5df73f`, notably
[moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686),
[moq-dev/moq#9644](https://github.com/moq-dev/moq/pull/9644),
[moq-dev/moq#9577](https://github.com/moq-dev/moq/pull/9577),
[moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529), and
[moq-dev/moq#9564](https://github.com/moq-dev/moq/pull/9564).

Reproduce the bursty rows alone first (`--rows` or the harness's filter) on the Nightly runner
class, measure several runs at each end to separate the steps from noise, then bisect each step.

Public API: none expected. Wire: none expected.
