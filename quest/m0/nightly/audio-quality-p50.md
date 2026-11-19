# [S] Browser audio-quality lag ceilings match what the player corrects

## Goal

The Nightly `Audio quality` job's browser `av_lag_ms_p50` ceilings sit outside the lag the player deliberately leaves alone, so a row fails only when audio really drifts. It failed on 11-13, 11-14, 11-15 and 11-17 on `chromium-aac-44100-congestion: av_lag_ms_p50 2.7 > 1.3`.

## Plan

Cause: the 1.3 ms ceiling was set provisionally in [moq-dev/moq#8459](https://github.com/moq-dev/moq/pull/8459) from a few developer-machine runs. [moq-dev/moq#8722](https://github.com/moq-dev/moq/pull/8722) then made the player leave lag within ±5 ms (half a 10 ms stretch step) uncorrected, so where p50 settles inside that window is chance (runner: 0.4–18.2 ms across 11-13..11-17). #8722 re-measured the replay rows but left the browser rows to the runner.

Decided 2026-11-18 (maintainer, /quest-plan): ✅ set the browser p50 ceilings to the ±5 ms window now, then tighten from runner artifacts (`gh run download <id> -n audio-quality-run`) once a week of nights since #8722 exists (rejected: wait for a week of runner data first and apply `budgets.json`'s worst night + 25% + 20 ms rule, about 24 ms, leaving Nightly red meanwhile).

Also look at the Opus browser rows: several rest 7–11 ms behind on the runner, outside the ±5 ms window, and Opus congestion hit exactly its 8.3 ceiling on 11-17. Decide from the data whether that is a playout bug for [audio playout](/quest/m2/audio-playout/README.md) or a ceiling to set.

A developer machine can't reproduce the runner's numbers (every browser row sits about 30 ms behind locally, as `budgets.json` notes), so use runner artifacts.

Public API: none. Wire: none.
