# [M] moq-audio in the browser

## Goal

On wasm32, moq-audio gains browser backends:
- **Capture:** getUserMedia into an AudioWorklet.
- **Playback:** through an AudioWorklet.
- **Codecs:** encode and decode Opus and AAC through WebCodecs
  `AudioEncoder` and `AudioDecoder`.

The program bus and the capture, decode, and playback entry points work on
the web.

## Plan

The encode and decode seams become asynchronous. Today `Backend::encode`
returns exactly one packet per frame, synchronously, but WebCodecs queues
work, delivers output through a callback, and drains through a promise. The
new seam submits input, yields zero or more outputs as they arrive, and
flushes with backpressure. It also serves pipelined platform codecs such as
MediaCodec. Browser objects are not `Send`, so these backends run on the
browser runtime. Any change to a published entry point gets a `MIGRATE.md`
entry.

## Required

- [hang and moq-mux on wasm](/quest/m3/studio/web/wasm-mux.md) - moq-audio's media dependencies build for wasm32
- [A runtime trait replaces web-async](/quest/m2/runtime-trait.md) - spawn and time on the browser instead of tokio's runtime
