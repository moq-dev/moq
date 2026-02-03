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

## CMAF Import
For importing fMP4/CMAF/HLS files, see the `moq-mux` crate.

## Examples
- [Publishing a video track](examples/video.rs)
