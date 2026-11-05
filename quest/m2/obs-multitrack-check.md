# [S] OBS simulcast layers reach moq-watch from a live OBS, including the dock's Go Live

## Goal

A live OBS 32.2.2 run, recorded in the PR, shows the
`WHIPSimulcastTotalLayers` profile setting attaching two or more scaled
video encoders to `moq_output`, each published as its own rendition and
watched in `<moq-watch>`, both through the obs-websocket recipe in
`doc/bin/obs.md` and through the MoQ dock's Go Live, which honors the
profile's layer count instead of creating a single encoder.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7333](https://github.com/moq-dev/moq/pull/7333),
which finished the OBS multitrack quest as docs only.
[moq-dev/moq#7333](https://github.com/moq-dev/moq/pull/7333) derived the recipe from OBS and obs-websocket sources without running
OBS, so the manual run never happened.

- Manual check: follow `doc/bin/obs.md`'s Simulcast renditions section on
  OBS 32.2.2 (the version `cpp/obs/buildspec.json` pins). Fix the doc if a
  step is wrong.
- Dock: `MoQDock::CreateVideoEncoder` (`cpp/obs/src/moq-dock.cpp:1050`)
  creates one encoder and attaches it with `obs_output_set_video_encoder`
  (:1112), ignoring `Stream1/WHIPSimulcastTotalLayers`. Read the count from
  the profile and create the scaled layers with OBS's ladder math (layer
  `i` at `i/N` of the output size and bitrate), attached with
  `obs_output_set_video_encoder2`. This gives a GUI path to simulcast,
  since stock Settings > Stream can't select `moq_service`.
- Cover the dock's layers with `just obs test`; verify with
  `just obs compile` and the manual run.
- [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) (merged)
  added a multi-encoder ladder to the OBS output tests
  (`g_ladder` in `cpp/obs/test/moq-output-test.cpp`). Extend it to assert
  each rendition advertises its own bitrate in the catalog.

Public API: none. Wire: none.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - the shaped-uplink proof of the rung disable [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) added, which turns these layers off as the grant falls
