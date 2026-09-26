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
`fmp4::Export::with_fragment_duration` adds an explicit duration cap. A zero cap
emits one fragment per frame; video with unknown duration waits for the next
timestamp or endpoint marker. Audio and samples with explicit durations remain
immediate. CMAF audio
samples are always encoded as sync samples; the decoded `Frame::keyframe` marks
only the first audio sample of a MoQ group.

Each catalog track constructor returns one `container::Producer` that owns the
media stream and its catalog entry. `set` publishes or replaces its config,
`modify` edits the published config through a guard, and dropping the producer
retires the entry. Calling `modify` before the first `set` returns
`Error::NotPublished`. Container writes measure bitrate; importers can also
measure batch span or reorder delay for jitter. Locally encoded frames call
`container::Producer::flush(timestamp, Instant::now())`; jitter is the spread
above that track's own recent minimum lateness, and delay is how far that
minimum trails the earliest track on the same catalog. Both are published as
soon as they rise. `import::Track::discontinuity()` marks a source seek or
pause, clears partial input, and restarts the flush baseline without lowering
advertised values. It forwards the container timeline marker, so resumed
timestamps must continue forward on the broadcast clock. Generic imports remain
clock-free. Invalid or decreasing jitter or delay is rejected before the edit is
retained, including while the initial catalog is reserved.
Codec importers propagate catalog and media errors through their configuration
and frame-writing methods.

Data tracks go through the catalog too. `catalog.json_stream(track, config)`
(or `json_snapshot`, `binary_snapshot`, `binary_stream`) writes the track's
`json` or `binary` entry, measures an absent `bitrate` from the writes, and
retires the entry when the producer drops. To list the track in your own
section beside application fields, pass that section's entry instead of a
`json::Config` or `binary::Config`: any `RenditionConfig` that embeds the data
config through `AsMut`.

```rust
#[derive(Serialize, Deserialize, Clone)]
struct Mavlink {
    #[serde(flatten)]
    binary: hang::catalog::BinaryConfig, // mode, compression, bitrate, ...
    sysid: u8,
}

impl AsMut<hang::catalog::BinaryConfig> for Mavlink {
    fn as_mut(&mut self) -> &mut hang::catalog::BinaryConfig {
        &mut self.binary
    }
}

// Plus `RenditionConfig<Ext>` writing to `catalog.ext.mavlink`, a map
// serialized under the `com.example.mavlink` root key.
let binary = hang::catalog::BinaryConfig::new(hang::catalog::Mode::Stream);
let mut telemetry = catalog.binary_stream(track, Mavlink { binary, sysid: 1 })?;
telemetry.append(packet)?;
```

The producer sets the entry's `mode` and encodes the track with its
`compression`. Read it back from `Catalog<Ext>` and subscribe with
`catalog::Entry::new(name, &entry.binary)`.

The fMP4, MPEG-TS, and FLV importers publish the source's own timestamps unless
built with `live()`, which translates them onto the catalog's broadcast clock:
the first frame is live on arrival, every track of the input shares that one
mapping, and a source that restarts its timestamps continues forward after the
real idle gap. fMP4 passthrough rewrites each fragment's `tfdt` to match. Use
it for a live feed with its own zero; publish verbatim only when the catalog's
clock (`Config::with_clock`) already names the source's zero.

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
