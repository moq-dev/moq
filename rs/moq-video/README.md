# moq-video

Native video capture, encoding, and publishing for [Media over QUIC](https://github.com/moq-dev/moq).

Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio). Built on
[`ffmpeg-next`](https://crates.io/crates/ffmpeg-next), but the public API is
ffmpeg-free at the signature level (capture/encode internals that traffic in
ffmpeg frames are private), so an `ffmpeg-next` bump isn't a breaking change.

Two public entry points:

- `encode::publish_capture(broadcast, catalog, capture::Config, encode::Options)`
  captures a webcam (libavdevice: avfoundation / v4l2 / dshow), H.264-encodes it
  (preferring a hardware encoder, falling back to `libx264`), and publishes on
  demand: the camera opens only while a subscriber is watching. Screen capture
  would slot into the same `capture` module.
- `encode::Producer` publishes H.264 you encoded yourself, handling the catalog
  and framing via `moq_mux::codec::h264::Import`.

Used by `moq-cli`'s `capture` subcommand. Requires a system FFmpeg (libav\*).

The `decode` (consume) side, mirror of `moq-audio`'s `AudioConsumer`, is not
implemented yet.
