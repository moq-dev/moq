# [S] Carry surround Opus through CMAF

## Goal

An fMP4 Opus track whose `dOps` declares channel mapping family 1 imports with
its mapping table in the OpusHead description, and a family 1 description
exports to a `dOps` that keeps the table. Today both directions refuse it.

## Plan

`mp4-atom` 0.16 rejects a nonzero `dOps` mapping family at decode and always
encodes family 0, so the table has nowhere to live. Expose the family and table
on `mp4_atom::Dops` upstream, then build the head with `opus::Mapping::new` on
import and write the table from the parsed head on export, replacing the
`UnsupportedMappingFamily` refusal in `synthesize_audio_trak`.

Regression: a family 1 5.1 `dOps` round-trips through import and export with
its table intact.

## Required

- `mp4-atom` releases a `Dops` that carries the channel mapping family and table
- [Audio codecs](/quest/m1/audio-codecs/README.md) - `opus::Mapping` and a `Config::encode` that writes any family's table
