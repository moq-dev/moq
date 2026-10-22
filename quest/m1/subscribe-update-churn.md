# [XS] Check a physical camera for subscription update chatter

## Goal

Waiting on a person with a physical camera. A reported `moq.dev/publish`
session logged an endless alternating `subscribe update` for audio and video;
every source measured since then is quiet, and a camera is the one source left.
Delete this quest if the camera publish is quiet too.

## Plan

[moq-dev/moq#6305](https://github.com/moq-dev/moq/pull/6305) measured steady
SUBSCRIBE_UPDATEs per track at `5d5974f083` through `cdn.moq.dev`, 60 seconds
after a five-second warmup:

| Source | Track | Warmup updates | Measured updates |
| --- | --- | --- | --- |
| `demo/bbb.hang` | audio | 0 | 0 |
| `demo/bbb.hang` | video | 1 | 0 |
| Chromium fake camera | audio | 0 | 0 |
| Chromium fake camera | video | 6 | 0 |

To check, publish a physical camera and microphone from `moq.dev/publish`, then
watch that broadcast from another tab or device. In the viewer tab's DevTools
console, enable the Verbose level and filter on `subscribe update: id=`;
`js/net`'s Lite subscriber logs one line per written update with its track.
The publisher's own `subscribe update:` line can undercount, since it
coalesces updates decoded while its serving loop is busy. After the first few
seconds, count lines per track for a minute.

- Quiet (no steady stream of lines): delete this quest and its
  [m1](/quest/m1/README.md) entry.
- Chatter: the likely cause is `js/watch/src/media.ts` pushing every
  `maxDelay` change into `subscriber.update`, compared exactly by
  `js/net/src/lite/subscriber.ts`. Replace this check with a fix, such as
  rounding the subscription's `maxDelay` up to a power of two ms.
