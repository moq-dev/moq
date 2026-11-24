# [M] moq-audio in the browser

## Goal

moq-audio runs in the browser for the
[web studio](https://github.com/moq-dev/studio/blob/main/quest/m2/web.md) in moq-dev/studio:
- **Capture:** getUserMedia into an AudioWorklet.
- **Playback:** through an AudioWorklet.
- **Codecs:** encode and decode Opus and AAC through WebCodecs
  `AudioEncoder` and `AudioDecoder`.

The program bus and the capture, decode, and playback entry points work on
the web. A test in a headless browser proves capture from a fake device,
mixing on the bus, and an encode and decode round trip.

## Plan

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): the studio's
web build moved from its m0 to its m2, so this stays deferred in m5 with
[moq-video in the browser](/quest/m5/video-web.md). The studio waits on it
through its
[browser audio pin quest](https://github.com/moq-dev/studio/blob/main/quest/m2/pin-browser-audio.md).

Changed 2026-11-02 (maintainer): the studio moved to moq-dev/studio, so this
lands here first with its own browser test rather than in the same PR as the
studio page's audio slice.

The encode and decode seams become asynchronous.
- **Why:** today `Backend::encode` returns exactly one packet per frame,
  synchronously, but WebCodecs queues work, delivers output through a
  callback, and drains through a promise.
- **New seam:** it submits input, yields zero or more outputs as they
  arrive, and flushes with backpressure. It also serves pipelined platform
  codecs such as MediaCodec.
- **Runtime:** browser objects are not `Send`, so these backends run on the
  browser runtime.
- **API changes:** any change to a published entry point says how to migrate
  in the PR's Impact bullet.

## Required

- [moq-video in the browser](/quest/m5/video-web.md) - the restored wasm32 target and browser runtime this builds on

## Related

- [WebCodecs audio priming](/quest/m3/webcodecs-priming.md) - measures each browser `AudioEncoder`'s priming; the WebCodecs backend's `delay()` reuses it
