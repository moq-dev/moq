# [S] The MoQ dock's Go Live publishes OBS simulcast layers

## Goal

A live OBS 32.2.2 run, recorded in the PR, shows the MoQ dock's Go Live
honoring the profile's `WHIPSimulcastTotalLayers` count instead of creating
a single encoder: two or more scaled video encoders attach to `moq_output`,
each published as its own rendition and watched in `<moq-watch>`.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7333](https://github.com/moq-dev/moq/pull/7333),
which finished the OBS multitrack quest as docs only.
[moq-dev/moq#7333](https://github.com/moq-dev/moq/pull/7333) derived the recipe from OBS and obs-websocket sources without running
OBS, so the manual run never happened.

Decided 2026-11-13 (maintainer, /quest-plan of `@moq/obs`): ✅ the
obs-websocket recipe's first live run moved to `@moq/obs`, which drives it
from code and fixes the doc (rejected: `@moq/obs` waits on this m3 quest).
This quest keeps the dock.
[moq-dev/moq#8721](https://github.com/moq-dev/moq/pull/8721) ran that
recipe on OBS 32.2.2 under Xvfb with Mesa's llvmpipe; its Validation has
the headless setup a live run of the dock can reuse.

- Dock: `MoQDock::CreateVideoEncoder` (`cpp/obs/src/moq-dock.cpp:1050`)
  creates one encoder and attaches it with `obs_output_set_video_encoder`
  (:1112), ignoring `Stream1/WHIPSimulcastTotalLayers`. Read the count from
  the profile and create the scaled layers with OBS's ladder math (layer
  `i` at `i/N` of the output size and bitrate), attached with
  `obs_output_set_video_encoder2`. This gives a GUI path to simulcast,
  since stock Settings > Stream can't select `moq_service`.
- Cover the dock's layers with `just obs test`; verify with
  `just obs compile` and the live run on OBS 32.2.2 (the version
  `cpp/obs/buildspec.json` pins).
- [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) (merged)
  added a multi-encoder ladder to the OBS output tests
  (`g_ladder` in `cpp/obs/test/moq-output-test.cpp`). Extend it to assert
  each rendition advertises its own bitrate in the catalog.

Public API: none. Wire: none.
