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

Public API: none. Wire: none.

## Related

- [fMP4 refusals keep framing](/quest/m1/security-audit/fmp4-refusal-framing.md) - the fMP4 leftover of the same streaming work
