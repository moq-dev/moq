# [XS] Try the VR demo in a headset and decide where it lives

## Goal

The maintainer has played `demo/vr` in a WebXR headset and decided whether
to promote it, such as hosting it beside the web demo at moq.dev with a VR180
broadcast to watch.

## Plan

The harness (`just test vr`) proves the layout in headless Chromium on a
desktop view only; a headset's per-eye render, recentre, and HRTF audio have
had no human check. Run it with `just relay`, the `moq import fmp4` command in
`doc/bin/demo.md#vr`, and `just vr serve`, then open the page on the headset
(it needs a secure context, so serve it over HTTPS or tunnel to localhost).

## Waiting

- Maintainer: try it in a headset (an agent has none), then pick: (a) host it at moq.dev with a looping VR180 broadcast, recommended once it plays well; (b) keep it local-only as a reference consumer. Check by this quest's Plan recording the pick.
