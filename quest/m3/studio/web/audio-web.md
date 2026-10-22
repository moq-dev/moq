# [M] moq-audio in the browser

## Goal

On wasm32, moq-audio gains browser backends:
- **Capture:** getUserMedia into an AudioWorklet.
- **Playback:** through an AudioWorklet.
- **Codecs:** encode and decode Opus and AAC through WebCodecs
  `AudioEncoder` and `AudioDecoder`.

The existing capture, encode, decode, and playback entry points and the
program bus work unchanged on the web.

## Required

- [hang and moq-mux on wasm](/quest/m3/studio/web/wasm-mux.md) - moq-audio's media dependencies build for wasm32
- [A runtime trait replaces web-async](/quest/m2/runtime-trait.md) - spawn and time on the browser instead of tokio's runtime
