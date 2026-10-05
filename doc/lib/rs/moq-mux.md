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
| MPEG-TS | yes | yes | H.264/H.265; AAC, MP2, AC-3, E-AC-3, Opus up to 7.1; SCTE-35 and subtitle PIDs carried as tracks; service tables round-trip; signalled timebase discontinuities preserved; paced export. |
| FLV / RTMP | yes | yes | Legacy H.264 + AAC + MP3, plus enhanced-RTMP HEVC, AV1, VP9, Opus, AC-3, E-AC-3, and multitrack. |
| Matroska / WebM | yes | yes | |
| Annex-B (H.264, H.265) | yes | yes | Parameter sets extracted to the catalog or re-injected per keyframe. |

Importers parse the bitstream to fill the catalog (resolution, codec string,
`description`), split groups at keyframes, and stamp timestamps. Exporters do
the inverse and skip stalled groups past a max age. Per-codec
producers (`import::Opus`, H.264, and so on) are available for feeding frames
you already have.

MPEG-TS `Import::stats` returns cumulative per-PID `StreamStats`: delivered
`units`, transport-clock `quiet` time, a `class` of audio, video, or data, audio
`resyncs`, scanned bytes `discarded`, frames `unconfirmed`, damaged units refused
in `damaged`, and the PID's share of the TR 101 290 counters. A malformed media
packet, PES header, or codec unit is dropped whole; only that PID loses sync, and
video closes its group at the break and waits for its next keyframe. Publishing
and catalog failures remain fatal. `ts::stats::Log` reports these counters for
both the CLI and SRT gateway, and grades a stopped stream for audio and video
only; a sparse data PID such as SCTE-35 stays in the row and is not logged for a
quiet second. The exporter's `damaged` count remains zero.

fMP4 export emits one fragment per publisher group by default, including audio.
A closed group flushes even if the live publisher pauses before its next frame.
`fmp4::Export::with_fragment_duration` adds an explicit duration cap. A zero cap
emits one fragment per frame; video with unknown duration waits for the next
timestamp or endpoint marker. Audio and samples with explicit durations remain
immediate. CMAF audio
samples are always encoded as sync samples; the decoded `Frame::keyframe` marks
only the first audio sample of a MoQ group.

`fmp4::Export` writes its init segment once every rendition can be described,
queueing other tracks' fragments (up to 30 seconds) behind it. The track set is
then fixed. A rendition that returns with the same sample entry reuses its track
id; `fmp4::Error::TrackAdded`, `TrackChanged`, and `TrackRewound` end the export
for a new rendition, a changed sample entry, or a replay of media already
written, and `TrackUndescribed` names a track that never delivered its codec
configuration. An Opus entry synthesized without a catalog `description` guesses
its pre-skip and input sample rate, so a later OpusHead that agrees on everything
else settles it instead of changing it. An OpusHead whose channel count
contradicts its catalog entry fails with `fmp4::Error::OpusChannelCount`.

Each catalog track constructor returns one `container::Producer` that owns the
media stream and its catalog entry. `set` publishes or replaces its config,
`modify` edits the published config through a guard, and dropping the producer
retires the entry. Calling `modify` before the first `set` returns
`Error::NotPublished`. Container writes measure bitrate; importers can also
measure batch span or reorder delay for jitter. Locally encoded frames call
`container::Producer::flush(timestamp, Instant::now())`; jitter is the spread
above that track's own recent minimum lateness, and delay is how far that
minimum trails the earliest track on the same catalog. The first rise publishes
the catalog at once; later rises within a second stay in the catalog and go out
with the first frame after that second, or with any earlier structural edit.
There is no timer, so a rise held when media stops waits for the next frame,
and `finish` does not publish it.
`import::Track::discontinuity()` marks a source seek or
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
    config: hang::catalog::BinaryConfig, // mode, compression, bitrate, ...
    sysid: u8,
}

impl AsMut<hang::catalog::BinaryConfig> for Mavlink {
    fn as_mut(&mut self) -> &mut hang::catalog::BinaryConfig {
        &mut self.config
    }
}

// Plus `RenditionConfig<Ext>` writing to `catalog.ext.mavlink`, a map
// serialized under the `com.example.mavlink` root key.
let config = hang::catalog::BinaryConfig::new(hang::catalog::Mode::Stream);
let mut telemetry = catalog.binary_stream(track, Mavlink { config, sysid: 1 })?;
telemetry.append(packet)?;
```

The producer sets the entry's `mode` and encodes the track with its
`compression`. Read it back from `Catalog<Ext>` and subscribe with
`catalog::Entry::new(name, &entry.config)`.

A payload timed on the broadcast clock is written at that timestamp as given,
and the entry advertises `jitter` and `delay` the way a media rendition does, so
telemetry lagging its video shows up as `delay`. A capture `Instant` (a
datagram's arrival, a sensor read) converts with `Clock::capture` on the
catalog's clock, which refuses an instant ahead of now. A timestamp ahead of now
is published anyway and measured as zero delay, so a source clock running
slightly fast is not rejected.

```rust
let at = catalog.clock().capture(received_at)?;
telemetry.append(moq_net::Timed::from(packet).at(at))?;
```

A timestamp carried over from elsewhere is published unchanged too: a source's
own timestamp on a catalog clock anchored to that source (KLV beside video from
one MPEG-TS program), or the `at` of a consumed `Timed` the payload was derived
from. It lines up with media only on a broadcast sharing the source's clock
mapping. A device's own clock is an unrelated epoch; keep it in the payload.

The fMP4, MPEG-TS, FLV, and MKV importers publish the source's own timestamps
(MPEG-TS after unwrapping its 33-bit PTS; fMP4 passthrough keeps each `tfdt`)
and anchor the catalog's broadcast clock instead: the first frame's timestamp
maps to the time it arrived, and every track of the input, like every importer
sharing the catalog, keeps that one mapping. Each importer withholds the
catalog until that first frame, so its first snapshot already carries the
anchored root `clock` for readers that copy it once. Data tracks stamp on the
clock too, even one created before that first frame, though anything it wrote
earlier stays on the clock the catalog started with. A clock set with
`Config::with_clock` is never re-anchored, for a recording whose zero names its
real start.

Group starts never go backwards. A group starting before the previous group's
start ends the import with `TimestampRewind`, whose `timestamp` and `floor` fields
name the refused frame and the previous group's start; the message gives both in
microseconds. A restarted encoder or a looping file wrapping to the top does this,
flagged MPEG-TS discontinuity or not; republish it as a new broadcast. Frames may
still dip below the previous group's content:
B-frames, and a keyframe overlapping the previous group's last frame. A flagged
MPEG-TS discontinuity that jumps forward continues the broadcast.

```bash
cargo add moq-mux
```

API: [docs.rs/moq-mux](https://docs.rs/moq-mux). Real-world usage:
[`rs/moq-cli`](https://github.com/moq-dev/moq/tree/main/rs/moq-cli).

Container producers and consumers take a format configured from the track's audio
or video catalog entry (`catalog::hang::Container::try_from(&config)`). For a raw
track, supply `container::Kind` explicitly. `cut(Some(end))` flushes and closes the
group immediately. Legacy and LOC video write an empty timestamped frame at that end;
audio and CMAF do not. With no explicit end, the producer uses a known sample
duration or observed cadence, independently of batching and reorder jitter.
Streaming consumers deliver frames immediately. The live fMP4 exporter receives
duration endpoints as metadata and uses them to time samples still buffered when
the group closes. Fetched groups retain their trailing frame until the marker or
group completion arrives.
