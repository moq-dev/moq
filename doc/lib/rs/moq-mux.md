---
title: moq-mux
description: Container import and export
---

# moq-mux

[![crates.io](https://img.shields.io/crates/v/moq-mux)](https://crates.io/crates/moq-mux)
[![docs.rs](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux)

Turns existing container formats into hang broadcasts and back. This is what
`moq import`/`export` and the gateways are built on.

| Format | Import | Export | Notes |
| --- | --- | --- | --- |
| fMP4 / CMAF | yes | yes | Passthrough as `cmaf` or repackaged as `legacy`. |
| MPEG-TS | yes | yes | H.264/H.265; AAC, MP2, AC-3, E-AC-3; SCTE-35 and subtitle PIDs carried as tracks; service tables round-trip; paced export. |
| FLV / RTMP | yes | yes | Legacy H.264 + AAC + MP3, plus enhanced-RTMP HEVC, AV1, VP9, Opus, AC-3, E-AC-3, and multitrack. |
| Matroska / WebM | yes | yes | |
| Annex-B (H.264, H.265) | yes | yes | Parameter sets extracted to the catalog or re-injected per keyframe. |

Importers parse the bitstream to fill the catalog (resolution, codec string,
`description`), split groups at keyframes, and stamp timestamps. Exporters do
the inverse and skip stalled groups past a latency budget. Per-codec
producers (`import::Opus`, H.264, and so on) are available for feeding frames
you already have.

```bash
cargo add moq-mux
```

API: [docs.rs/moq-mux](https://docs.rs/moq-mux). Real-world usage:
[`rs/moq-cli`](https://github.com/moq-dev/moq/tree/main/rs/moq-cli).
