# [S] TS Opus import refuses or parses extended channel codes

## Goal

An MPEG-TS Opus stream whose extension descriptor carries a
`channel_config_code` of 0x81 or above imports with its real OpusHead or is
refused. It never decodes as a guessed stereo stream, which misreads every
multistream packet.

## Plan

- The importer falls back to stereo for codes it does not know. 0x81 is
  specified as an explicitly coded layout (channel count, mapping family, stream
  counts, and table in the descriptor), and ffmpeg's muxer also writes
  `0x80 | channels` for an alternate table its own demuxer does not read. Check
  the Opus-in-TS spec and ffmpeg before deciding which codes to parse.
- Build parsed layouts with the public `opus::Mapping::new`, which validates
  the table; refuse any code that is reserved or does not parse, per stream,
  without failing the rest of the program.
- Fixtures from a real muxer where one exists; a hand-built descriptor
  otherwise, noted as such.

Public API: none. Wire: none.

## Related

- [TS Opus export refusals](/quest/m1/audio-codecs/ts-opus-export-refusals.md) - the exporter side of the same descriptor
