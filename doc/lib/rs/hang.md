---
title: hang
description: The media layer
---

# hang

[![crates.io](https://img.shields.io/crates/v/hang)](https://crates.io/crates/hang)
[![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)

The [hang media format](/concept/hang) on top of `moq-net`: a live catalog
describing renditions with WebCodecs-style decoder configs, and containers
that carry a timestamp with every frame.

- **Catalog producer and consumer.** Typed `VideoConfig` and `AudioConfig` per rendition, shared video properties (display size, rotation, flip), stalled hints, cross-broadcast rendition references, and timeline tracks. Extend it with your own sections through a lock or `#[serde(flatten)]`, or read unknown sections as raw JSON.
- **Containers.** `legacy`, `cmaf`, and `loc`, decoded for you; unknown kinds pass through untouched.
- **`OrderedConsumer`** reads a media track as timestamped frames, reorders groups, and skips ones that fall past your latency budget.
- **Codecs described**: H.264, H.265, VP8, VP9, AV1, AAC, Opus, PCM.

```bash
cargo add hang
```

Producing media is usually done through [`moq-mux`](/lib/rs/moq-mux) (from a
container) or [`moq-video`](/lib/rs/moq-video) and
[`moq-audio`](/lib/rs/moq-audio) (from a device), which build the catalog for
you. Examples:
[`video.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs)
and
[`subscribe.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs).
API: [docs.rs/hang](https://docs.rs/hang). The TypeScript twin is
[`@moq/hang`](/lib/js/hang).
