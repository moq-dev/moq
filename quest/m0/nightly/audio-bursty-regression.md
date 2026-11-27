# [M] The bursty audio rows pass their budgets again

## Goal

Nightly's `Audio quality` (`just test audio-quality --runtimes chromium --enforce`) passes the
`chromium-opus-48000-bursty` and `chromium-aac-44100-bursty` rows on a scheduled run on
`main`, fixed at the cause, without loosening a budget.

## Plan

Found by the Nightly triage of 2026-11-26. Both rows passed in run
[45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582) (`5d86f05321`), then
regressed in two steps.

The 11-26 step, mid-run underruns and trims that grow with the run's length, is fixed by
[moq-dev/moq#9746](https://github.com/moq-dev/moq/pull/9746): a JS arrival read kept a fill
verdict for every skipped stretch since its join and judged them all again on each budget
change, so main-thread long tasks grew to a second. What remains is the 11-25 step, seen only on
the Nightly runner (`ubuntu-24.04-arm`):

- About 3 s after the join, arrivals stop for 300 to 800 ms and then land together. The opus
  row's target jumps to 700 to 900 ms, holds for one 20 s half-life, and trims about 450 ms when
  it falls, failing `trimmed_ms_per_min` and `target_ms_p95` in about half the runs (the bisect
  runs [45910365489](https://github.com/moq-dev/moq/actions/runs/45910365489),
  [45931921427](https://github.com/moq-dev/moq/actions/runs/45931921427), and
  [45950231382](https://github.com/moq-dev/moq/actions/runs/45950231382) among them). Nightlies
  up to 11-24 had no such gap.
- `native-opus-48000-bursty` has failed `trimmed_ms_per_min` and `target_ms_p95` since the same
  night, so the cause may sit below both players (relay or transport).
- The aac row failed only once, `av_lag_ms_p50` 8.2 on 11-25, and passed after.

Six local runs on an x86_64 developer host never showed the startup gap, before or after
[moq-dev/moq#9561](https://github.com/moq-dev/moq/pull/9561). That PR cut the harness's opus
publisher from 50 group streams a second to 10, which may remove the gap too. Read the first
scheduled Nightly with both PRs first. If the gap remains, bisect `5d86f05321..420b8dcb0d` on
the runner class with the bursty rows alone (`--profiles bursty --capture`), comparing each
run's arrival gaps, since a single row's budgets are too noisy to bisect on. Candidates:
[moq-dev/moq#9549](https://github.com/moq-dev/moq/pull/9549) (a hold keeps the stretch's
pace), [moq-dev/moq#9572](https://github.com/moq-dev/moq/pull/9572) (retained audio replays
only for a live subscription), [moq-dev/moq#9562](https://github.com/moq-dev/moq/pull/9562)
and [moq-dev/moq#9578](https://github.com/moq-dev/moq/pull/9578) (Opus concealment).

Public API: none expected. Wire: none expected.

## Waiting

- moq-dev/moq: the first scheduled Nightly on `main` with moq-dev/moq#9561 and moq-dev/moq#9746 runs the bursty rows; check its `Audio quality` job for the chromium and native bursty rows, and its `audio-quality-run` artifact's opus arrivals for a gap after the join
