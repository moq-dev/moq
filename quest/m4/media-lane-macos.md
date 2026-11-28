# [M] The media lane passes on macOS

## Goal

`just test media` passes on an Apple Silicon host as it does on Linux CI, so a media flake can be
reproduced from a macOS agent host instead of only on a Linux runner.

Today the fixture's audio never reaches the player in time on macOS. At `d78c96c2e6` (Chromium
156, headless), `just test media --media-cases mute-volume` failed two ways in two runs. One
failed `playback starts`: the ring never filled, the relay served the fixture's 100 ms audio
groups about 400 ms apart, and the catalog advertised a 586 ms delay. The other failed
`volume 1: only 0/30 samples carried audio`: the player's audio bytes stood still for the whole
1.5 s window while its delay sat at 2.6 s. Both pages also logged a long animation frame
(57-138 ms, no script) about every 100 ms.

## Plan

The fixture's audio-hold log (`AUDIO_HOLDS` in `test/interop/clients/js/src/contract.ts`) saw
only one 102 ms encoder hold in that window. Capture and encoder kept up, so start after the
encoder: the JS group writes and Chromium's WebTransport send. Check whether the steady long
frames (headless Chromium without a GPU adapter, painting through Canvas2D) starve that path,
and whether a headed or GPU-backed launch on macOS changes it. Fix at the cause; if it is a
headless macOS limitation, the lane fails loud on macOS with that reason instead of timing out.

Public API: none. Wire: none.

## Hosts

- macos - the failure is macOS-only
