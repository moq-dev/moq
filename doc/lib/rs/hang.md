---
title: hang
description: The media layer
---

# hang

[![crates.io](https://img.shields.io/crates/v/hang)](https://crates.io/crates/hang)
[![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)

The [hang media format](/concept/hang) on top of `moq-net`: the catalog that
describes renditions with WebCodecs-style decoder configs, and the frame that
carries a timestamp with each payload.

- **Catalog.** `Catalog` types video, audio, and text renditions, the `json` and `binary` data tracks, and the `archive` entry. Add your own sections through its type parameter, or read unknown ones as raw JSON. `Catalog::subscribe(&broadcast)` follows it live.
- **Frames.** `container::Frame` encodes and decodes the `legacy` container, and `container::track_info` declares a media track. A rendition may instead declare `cmaf` or `loc`; an unknown container kind is kept rather than failing the catalog.
- **Codecs described**: H.264, H.265, VP8, VP9, AV1, AAC, Opus, PCM, FLAC, MP3, MP2, AC-3, E-AC-3.

```bash
cargo add hang
```

hang is the format, not the pipeline. [`moq-mux`](/lib/rs/moq-mux) produces the
catalog for you and decodes any container in group order
(`moq_mux::container::Consumer`), skipping groups that fall past your max delay.
[`moq-video`](/lib/rs/moq-video) and [`moq-audio`](/lib/rs/moq-audio) publish
from a device. Examples:
[`video.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs)
and
[`subscribe.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs).
API: [docs.rs/hang](https://docs.rs/hang). The TypeScript twin is
[`@moq/hang`](/lib/js/hang).
