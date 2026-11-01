---
title: moq-mux
description: Container import and export
---

# moq-mux

[![crates.io](https://img.shields.io/crates/v/moq-mux)](https://crates.io/crates/moq-mux)
[![docs.rs](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux)

Turns existing container formats into [hang](/concept/hang) broadcasts and
back. This is what `moq import` / `export` and the gateways are built on.

| Format | Import | Export | Notes |
| --- | --- | --- | --- |
| fMP4 / CMAF | yes | yes | Audio/video unwrapped to `legacy` by default, or passed through as `cmaf` with `with_passthrough`; `wvtt` subtitles import as timed WebVTT text and `stpp` TTML (IMSC timing profile) as `ttml` documents on the broadcast clock, preserving source language and label when present; other subtitle codecs are skipped. |
| MPEG-TS | yes | yes | H.264/H.265; AAC, MP2, AC-3, E-AC-3, Opus up to 7.1; SCTE-35 sections and every other undecoded PES stream (KLV, subtitles, DVB AC-3) as tracks; service tables round-trip. |
| FLV / RTMP | yes | yes | Legacy H.264 + AAC + MP3, plus enhanced-RTMP HEVC, AV1, VP9, Opus, AC-3, E-AC-3, and multitrack. |
| Matroska / WebM | yes | yes | Import streams each block into its frame in one pass, retaining no declared size. It refuses subtitle tracks, CMAF output, and video `BlockGroup`s with fields after their `Block`: remux video as `SimpleBlock`s and extract subtitles separately. |
| Annex-B (H.264, H.265) | yes | yes | Parameter sets extracted to the catalog or re-injected per keyframe. |

Importers fill the catalog from the bitstream, split groups at keyframes, and
publish the source's own timestamps, mapped to the wall time the first frame
arrived. The catalog clock never moves once something uses it: an importer that
starts after a data track, a capture, another importer, or a pinned clock
shifts its timestamps onto that clock instead, landing its first frame at now.
A `catalog::Timebase` gives renditions on one timestamp base one shared shift. A
stream whose decoder identity changes, such as an SPS with a new profile or level
or an audio header with a new sample rate, continues under a new track name
(`<name>/<n>` for the codec importers) while the old track ends; everything else,
such as the coded size, updates the rendition in place. A
timeline that rewinds ends the import. The MPEG-TS import ends at a clock step
onto another timeline with `ts::Restart`, which carries the input from the step
for the caller to publish as a new broadcast. fMP4 export fixes its track
set at the init segment, so a new rendition or a changed configuration ends the
export. The [CLI page](/bin/cli#import) covers these and the MPEG-TS specifics:
one program per broadcast, private PES, damaged packets, and feed checks.

Exporters take a catalog stream alongside the source. Choose its encoding and
rendition selection before exporting; the same stream shape drives every container.
MPEG-TS metadata is retained when the stream uses `ts::Ext`.

A container read yields frames plus two markers in delivery order: a gap where
media after what was read will never arrive (a skip, a hole), carrying where it
starts and how long it lasts as an upper bound, and an end where the publisher
declared its media stops (a pause, a source switch, the finish). Joining
mid-track is not a gap.

Data tracks are catalog entries too. The [hang page](/concept/hang#data-tracks)
describes the modes. A capture time converts onto the broadcast clock; a
timestamp taken from the source is published as given and lines up with media
only when they share that clock.

```bash
cargo add moq-mux
```

API: [docs.rs/moq-mux](https://docs.rs/moq-mux). The commands that exercise it
are [`moq-cli`](/bin/cli).
