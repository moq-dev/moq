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
carries every metadata block, so it can hold embedded cover art; size its
limit for that or refuse such art explicitly. Pick each limit from its spec
and the largest real decoder config, check them against the MKV fixtures
for each codec, and return the
importer's existing error for a refused element. Test each element declared
past its limit, fed in small chunks.

Research checkpoint 2026-11-02, implementation paused for a supported-feature
choice in [moq-dev/moq#7629](https://github.com/moq-dev/moq/pull/7629):

- Reproduced all three unbounded leaves with a 1 GiB declaration and 4096
  incoming bytes, split into 1-, 7-, and 64-byte chunks. Reader retention
  rose from 12 to 4108 bytes in each case instead of refusing the header.
- Fresh ffmpeg 9.0.2 Matroska fixtures were accepted in those same chunk
  sizes for AVC, HEVC, AV1, Opus, AAC, and FLAC. Their actual CodecPrivate
  element sizes were 44, 2478, 16, 19, 5, and 42 bytes respectively. These
  are measured examples, not maximum accepted configurations. In
  particular, ffprobe reports FLAC extradata as 34 bytes, excluding the
  marker and metadata header that make its Matroska element 42 bytes.
- [AV1 ISOBMFF sections 2.3.3 and 2.3.4](https://aomediacodec.github.io/av1-isobmff/#av1codecconfigurationbox-syntax)
  define an open-ended configOBUs array. Only the sequence-header count is
  bounded; arbitrary metadata and padding do not yield a small total-size
  maximum. The current importer retains all configOBUs in the description.
- [RFC 9639 sections 6 and 8.1](https://www.rfc-editor.org/rfc/rfc9639.html#section-8.1)
  allow any number of FLAC metadata blocks, each with a 24-bit body length.
  A single-block maximum does not bound the complete CodecPrivate, even
  after refusing pictures. The current importer passes all metadata into
  the catalog description, while parsing only STREAMINFO.
- [RFC 7845 section 5.1](https://www.rfc-editor.org/rfc/rfc7845.html#section-5.1)
  gives the known OpusHead layout (19 bytes, or 21 plus up to 255 channel
  mappings), but also permits extra fields for compatible minor versions.
  The current parser accepts minor versions 0 through 15 and the importer
  retains trailing data. A 276-byte cap would restrict that support.
- [Matroska ordering](https://www.matroska.org/technical/ordering.html)
  does not mandate CodecID before CodecPrivate. The current importer
  resolves the codec at TrackEntry end. A codec-specific refusal at the
  private-data header therefore also needs a decision about out-of-order
  entries, rather than guessing the codec from private bytes.

Decisions still needed: define the supported bounded decoder-config subset
(including AV1 metadata, FLAC metadata beyond STREAMINFO, and Opus minor
extensions), or authorize a streaming/canonicalization design instead of
retaining each original decoder config whole. Also decide whether
CodecPrivate preceding CodecID is explicitly unsupported. No narrowing of
these features has been selected, and no production change has been made.

Public API: none. Wire: none.

## Related

- [fMP4 refusals keep framing](/quest/m1/fmp4-refusal-framing.md) - the fMP4 leftover of the same streaming work
