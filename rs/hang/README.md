[![Documentation](https://docs.rs/hang/badge.svg)](https://docs.rs/hang/)
[![Crates.io](https://img.shields.io/crates/v/hang.svg)](https://crates.io/crates/hang)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](../../LICENSE-MIT)

# hang

A media library built on top of `moq-lite` for streaming audio and video.
`hang` provides media-specific functionality, split into a few components:

- **Broadcast**: A discoverable collection of tracks, documented using a catalog.
- **Catalog**: Metadata describing the available tracks, codec information, etc. This is a live track itself and is updated as tracks are added/removed/changed.
- **Track**: Audio/video streams, as well as other types of data.
- **Group**: A group of pictures (video) or collection of samples (audio). Each group is independently decodable.
- **Frame**: A timestamp and a codec payload pair.

## Supported Codecs
We most of the implement the [WebCodecs specification](https://www.w3.org/TR/webcodecs/#video-decoder-config).

- **Video:** H.264, H.265, VP8, VP9, AV1
- **Audio:** AAC, Opus

## Native Playback (Optional)

`hang` now supports native playback through optional feature flags:

- **`decode`**: Decode compressed frames using FFmpeg (requires system FFmpeg)
- **`render`**: GPU-accelerated video rendering using wgpu
- **`playback`**: Both decode and render features

```toml
[dependencies]
hang = { version = "0.10", features = ["playback"] }
```

See [PLAYBACK.md](PLAYBACK.md) for detailed documentation on native playback support.

## CMAF Import
There's also a `cmaf` module that can import fMP4/CMAF files into a hang broadcast.
It's crude and doesn't support all features, but it's a good starting point to ingest existing content.

## Examples
- [Publishing a video track](examples/video.rs)
