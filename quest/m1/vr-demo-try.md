# [XS] Try the VR demo in a headset, then host it at moq.dev

## Goal

Someone has played `demo/vr` in a WebXR headset, and once it plays well
there, it is hosted beside the web demo at moq.dev with a looping VR180
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

Decided 2026-11-15 (maintainer, /quest-complete): ✅ ask the contributor who
started the demo in [moq-dev/moq#6373](https://github.com/moq-dev/moq/pull/6373)
to try it in a headset, rather than the maintainer.

## Waiting

- nuts-rice: try `demo/vr` in a WebXR headset (an agent has none) and say whether the per-eye render, recentring, and ambisonic audio play well enough to host, as asked in [moq-dev/moq#8781](https://github.com/moq-dev/moq/pull/8781#issuecomment-6901206041) on 2026-11-15. Check by reading that thread for a reply; this quest's Plan records the headset result.
