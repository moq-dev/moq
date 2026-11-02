# [XS] MKV leaf element limits

## Goal

The MKV importer refuses a `CodecID`, `DocType`, or `CodecPrivate` element
whose declared size is past a limit specific to that element, so a header
declaring a huge one cannot pin importer memory.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234),
which streams blocks and discards ignored elements but still reads these
three leaf elements whole (`rs/moq-mux/src/container/mkv/import.rs`, where
the track entry collects `CodecID` and `CodecPrivate`). #7234 has landed.

The maintainer earlier ruled out a blanket tag-size cap (it would refuse
large Segments and Clusters). These limits are per element
and bound only what the importer must hold whole: a `CodecID` and `DocType`
are short ASCII strings, and `CodecPrivate` is the decoder config of a
codec the importer supports: avcC (`V_MPEG4/ISO/AVC`), hvcC
(`V_MPEGH/ISO/HEVC`), av1C (`V_AV1`), an OpusHead (`A_OPUS`), an AAC
AudioSpecificConfig (`A_AAC`), or a FLAC header (`A_FLAC`). The FLAC header
carries every metadata block, so it can hold embedded cover art. Pick each
limit from its spec's supported configuration (decided below), check it
against the MKV fixtures for each codec, and refuse an element past it. Test each element declared
past its limit, fed in small chunks.

Research checkpoint 2026-11-02 in [moq-dev/moq#7629](https://github.com/moq-dev/moq/pull/7629):
all three leaves reproduced unbounded (a 1 GiB declaration retained every
byte fed), and fresh ffmpeg 9.0.2 MKVs for AVC, HEVC, AV1, Opus, AAC, and
FLAC import in 1-, 7-, and 64-byte chunks. Their sizes are examples, not
limits.

Decided 2026-11-02 (maintainer, the /quest-complete session):

- Decoder configs: define and document a bounded supported configuration per
  codec, refusing excluded metadata and extensions at the CodecPrivate header,
  before their bodies are read. No size picked from sampled fixtures, and no
  streaming or canonicalizing redesign. Reason: AV1 configOBUs, FLAC metadata
  chains, and OpusHead minor-version fields have no finite aggregate bound, so
  only an explicit supported subset bounds them without an arbitrary constant.
  FLAC is the `fLaC` marker plus STREAMINFO alone (42 bytes). Opus is the RFC
  7845 layout (19 bytes, or 21 plus the channel mapping), without trailing
  fields. AV1 is the av1C header plus at most a sequence header, first, and
  one each of the HDR content light and mastering display metadata that
  `color` reads, sized from the AV1 syntax maxima.
- Element order: refuse a CodecPrivate before its CodecID, and a second
  CodecID. Reason: the codec must pick the supported path before the private
  bytes are buffered; arbitrary order would need staging or canonicalization.

Implemented in #7629: DocType and CodecID are bounded by the longest supported
value (a longer CodecID is recorded by its length, so skipped track types still
pass), and codecs that ignore CodecPrivate (VP8, VP9, MP3, unsupported ones)
skip it as it arrives.

Decided 2026-11-02 (the maintainer had no preference and took the
recommendation, /quest-complete session), for the codecs the first decision
left read whole without a bound:

- ✅ (a) Canonicalize. HEVC discards hvcC SEI arrays as they arrive and keeps
  only the parameter sets, bounded by their id spaces (16 VPS, 16 SPS, 64
  PPS, each up to 64 KiB). AVC bounds avcC by its count fields. AAC takes the
  AudioSpecificConfig syntax maximum and refuses unparsed trailing bytes.
- (b) Treat hvcC SEI like AV1 metadata: keep the HDR SEI types and refuse the
  rest, which refuses default x265 output.
- (c) Bound all three by what one catalog frame carries (moq-net's 32 MiB
  group cache): simple, but loose.

Reason: ffmpeg's libx265 hvcC carries a 2365-byte user-data SEI (x265's
encoder settings) and hvcC SEI arrays have no finite bound, so refusing them
would refuse default ffmpeg+x265 MKVs. Canonicalizing keeps every real file
importing without inventing a constant.

Public API: two `mkv::Error` variants. Wire: none.

## Related

- [fMP4 refusals keep framing](/quest/m1/fmp4-refusal-framing.md) - the fMP4 leftover of the same streaming work
