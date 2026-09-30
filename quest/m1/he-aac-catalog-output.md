# [S] HE-AAC catalog entries describe the output

## Goal

A catalog entry for HE-AAC or HE-AACv2 names what the stream plays as, for
example 48 kHz stereo, not its AAC-LC core (24 kHz, and mono for v2). Players
and gateways that size buffers or pick a decoder from the catalog see the real
output.

## Plan

- `Config::parse` reports the core rate and channel count because those lead
  an explicit SBR or PS AudioSpecificConfig. The FLV and MKV importers copy
  them into the catalog; fMP4 takes the sample entry instead, so the importers
  disagree for the same stream.
- Derive the output from the config: the extension rate under SBR, two
  channels under PS. Implicit SBR, found only in band, cannot be known from the
  config; keep the documented half-rate behavior for it.
- Consumers that rebuild a config from catalog fields (description synthesis in
  `moq-audio`, the MSF path) must not then mistake the output rate for the core.
- Regression: the `fdkaacenc` HE-AAC and HE-AACv2 FLV fixtures under
  `rs/moq-mux/src/container/ts/test_data` import as 48 kHz stereo, matching
  ffprobe.

Public API: possibly `Config` fields. Wire: catalog values change for HE-AAC.

## Related

- [#4283](https://github.com/moq-dev/moq/pull/4283) - found while labeling HE-AAC for ADTS
