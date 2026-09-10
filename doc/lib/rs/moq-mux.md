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
| MPEG-TS | yes | yes | H.264/H.265; AAC, MP2, AC-3, E-AC-3; SCTE-35 and subtitle PIDs carried as tracks; service tables round-trip; signalled timebase discontinuities preserved; paced export. |
| FLV / RTMP | yes | yes | Legacy H.264 + AAC + MP3, plus enhanced-RTMP HEVC, AV1, VP9, Opus, AC-3, E-AC-3, and multitrack. |
| Matroska / WebM | yes | yes | |
| Annex-B (H.264, H.265) | yes | yes | Parameter sets extracted to the catalog or re-injected per keyframe. |

Importers parse the bitstream to fill the catalog (resolution, codec string,
`description`), split groups at keyframes, and stamp timestamps. Exporters do
the inverse and skip stalled groups past a max age. Per-codec
producers (`import::Opus`, H.264, and so on) are available for feeding frames
you already have.

fMP4 export emits one fragment per publisher group by default, including audio.
A closed group flushes even if the live publisher pauses before its next frame.
`fmp4::Export::with_fragment_duration` adds an explicit duration cap. CMAF audio
samples are always encoded as sync samples; the decoded `Frame::keyframe` marks
only the first audio sample of a MoQ group.

`catalog::Rendition::set`, `update`, and `estimate` return errors when a catalog
edit cannot be serialized or published. Invalid jitter is rejected before the
edit is retained, including while the initial catalog is reserved. Codec importers
propagate these errors through their configuration and frame-writing methods.

```bash
cargo add moq-mux
```

API: [docs.rs/moq-mux](https://docs.rs/moq-mux). Real-world usage:
[`rs/moq-cli`](https://github.com/moq-dev/moq/tree/main/rs/moq-cli).

Container producers and consumers take a format configured from the track's audio
or video catalog entry (`catalog::hang::Container::try_from(&config)`). For a raw
track, supply `container::Kind` explicitly. `cut(Some(end))` flushes and closes the
group immediately. Legacy video writes an empty timestamped frame at that end;
audio and CMAF do not. With no explicit end, the producer uses a known sample
duration or observed cadence, independently of batching and reorder jitter.
Streaming consumers deliver frames immediately. The live fMP4 exporter receives
duration endpoints as metadata and uses them to time samples still buffered when
the group closes. Fetched groups retain their trailing frame until the marker or
group completion arrives.
