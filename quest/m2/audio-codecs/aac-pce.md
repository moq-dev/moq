# [S] moq-mux: an AAC channel_config of 0 parses the PCE or refuses the track

## Goal

The catalog never invents an AAC channel count. A stream whose
channelConfiguration is 0 gets its count from the program config element it
carries, and a reserved value refuses the track with a clear error. Today
both map to stereo with a warning.

## Plan

The ADTS and ASC importers in `rs/moq-mux/src/codec/aac` map
`channel_config == 0` and the reserved values 13 to 15 to stereo and warn.
Warn-then-continue is banned: supported or refused.

- ASC: with `channel_config == 0` the GASpecificConfig carries a
  `program_config_element`; parse its front, side, back, and LFE element
  counts into the channel count (and, once [Layout](/quest/m2/audio-codecs/layout.md)
  lands, into a layout).
- ADTS: channel_config 0 means the PCE is in the first raw data block. Parse
  it there, once per track, the same way the HE-AAC sniff reads the first
  block's fill elements.
- Reserved values are `Error::Unsupported`.
- Regression: an ASC fixture with a PCE reports its real count; an ADTS
  fixture with an in-band PCE does too; a reserved value is refused; the
  common configurations 1 to 7 are unchanged.

## Related

- [HE-AAC refusal](/quest/m2/audio-codecs/he-aac-refusal.md) - reads the same first block
- [Layout](/quest/m2/audio-codecs/layout.md) - what the parsed PCE eventually maps to
