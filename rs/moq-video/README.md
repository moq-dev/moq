# moq-video

Native video capture, encoding, and publishing for [Media over QUIC](https://github.com/moq-dev/moq).

Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio). Built on
[`ffmpeg-next`](https://crates.io/crates/ffmpeg-next):

- `Camera` captures a webcam via libavdevice (avfoundation / v4l2 / dshow).
- `Encoder` encodes decoded frames to Annex-B H.264, preferring a platform
  hardware encoder (`h264_videotoolbox` / `h264_nvenc` / `h264_vaapi`) and
  falling back to software (`libx264`).
- `VideoProducer` publishes encoded frames through `moq_mux::codec::h264::Import`.
- `publish_camera` is a one-call capture-encode-publish loop.

Used by `moq-cli`'s `webcam` subcommand. Requires a system FFmpeg (libav\*).

Decode/consume (the mirror of `moq-audio`'s `AudioConsumer`) is not implemented
yet.
