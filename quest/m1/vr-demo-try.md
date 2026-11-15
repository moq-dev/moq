# [XS] Try the VR demo in a headset, then host it at moq.dev

## Goal

The maintainer has played `demo/vr` in a WebXR headset, and once it plays
well there, it is hosted beside the web demo at moq.dev with a looping VR180
broadcast to watch.

## Plan

Decided 2026-11-14 (maintainer, pick round for
[moq-dev/moq#8781](https://github.com/moq-dev/moq/pull/8781)): ✅ host it at
moq.dev with a looping VR180 broadcast once it plays well in a headset
(rejected: keep it local-only as a reference consumer).

The harness (`just test vr`) proves the layout in headless Chromium on a
desktop view only; a headset's per-eye render, recentre, and HRTF audio have
had no human check. Run it with `just relay`, the `moq import fmp4` command in
`doc/bin/demo.md#vr`, and `just vr serve`, then open the page on the headset.
It needs a secure context, so serve it over HTTPS or tunnel it to the
headset's localhost, and make the relay reachable too: `localhost:4443` on the
headset is the headset itself, so forward that port as well or point the page
at the relay with `?url=`.

## Waiting

- Maintainer: try it in a headset (an agent has none) and say whether it plays well enough to host. Check by this quest's Plan recording the result.
