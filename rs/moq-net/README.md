[![Documentation](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net/)
[![Crates.io](https://img.shields.io/crates/v/moq-net.svg)](https://crates.io/crates/moq-net)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-net

The Rust networking layer for [Media over QUIC](https://moq.dev): real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC.

At session setup `moq-net` negotiates one of two wire protocols: the simplified [moq-lite](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) protocol (the default) or the full IETF [moq-transport](https://datatracker.ietf.org/group/moq/documents/) protocol. This means clients work with any moq-transport CDN.

Live media is built on top of this layer using something like [hang](https://github.com/moq-dev/moq/tree/main/rs/hang).

- **Broadcasts**: Discoverable collections of tracks.
- **Tracks**: Named streams of data, split into groups.
- **Groups**: A sequential collection of frames, usually starting with a keyframe.
- **Frame**: A timed chunk of data.

## Examples

- [Publishing a chat track](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs)
- [Publishing or consuming a clock track](https://github.com/moq-dev/moq/tree/main/rs/moq-clock)
