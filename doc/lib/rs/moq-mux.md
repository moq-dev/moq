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
| fMP4 / CMAF | yes | yes | Passthrough as `cmaf` or repackaged as `legacy`. |
| MPEG-TS | yes | yes | H.264/H.265; AAC, MP2, AC-3, E-AC-3, Opus up to 7.1; SCTE-35 and subtitle PIDs as tracks; service tables round-trip. |
| FLV / RTMP | yes | yes | Legacy H.264 + AAC + MP3, plus enhanced-RTMP HEVC, AV1, VP9, Opus, AC-3, E-AC-3, and multitrack. |
| Matroska / WebM | yes | yes | |
| Annex-B (H.264, H.265) | yes | yes | Parameter sets extracted to the catalog or re-injected per keyframe. |

Importers fill the catalog from the bitstream, split groups at keyframes, and
publish the source's own timestamps, mapped to the wall time the first frame
arrived. A timeline that rewinds ends the import. fMP4 export fixes its track
set at the init segment, so a new rendition or a changed configuration ends the
export. The [CLI page](/bin/cli#import) covers these and the MPEG-TS specifics:
one program per broadcast, damaged packets, and feed checks.

Data tracks are catalog entries too. The [hang page](/concept/hang#data-tracks)
describes the modes. A capture time converts onto the broadcast clock; a
timestamp taken from the source is published as given and lines up with media
only when they share that clock.

```bash
cargo add moq-mux
```

API: [docs.rs/moq-mux](https://docs.rs/moq-mux). The commands that exercise it
are [`moq-cli`](/bin/cli).
