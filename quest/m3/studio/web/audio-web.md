# [M] moq-audio in the browser

## Goal

The `/studio` page captures, mixes, plays back, and publishes audio. That
page is the consumer, so it lands in the same PR as moq-audio's browser
backends:
- **Capture:** getUserMedia into an AudioWorklet.
- **Playback:** through an AudioWorklet.
- **Codecs:** encode and decode Opus and AAC through WebCodecs
  `AudioEncoder` and `AudioDecoder`.

The program bus and the capture, decode, and playback entry points work on
the web.

## Plan

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

- [moq-video in the browser](/quest/m3/studio/web/video-web.md) - the `/studio` page this adds audio to
