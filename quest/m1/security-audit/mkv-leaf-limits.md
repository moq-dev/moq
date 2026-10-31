# [XS] MKV leaf element limits

## Goal

The MKV importer refuses a `CodecID`, `DocType`, or `CodecPrivate` element
whose declared size is past a limit specific to that element, so a header
declaring a huge one cannot pin importer memory.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234),
which streams blocks and discards ignored elements but still reads these
three leaf elements whole. Starts after #7234 lands; it is a queued PR that
finishes and deletes the `mkv-stream-blocks` quest, so it is named here
rather than linked under Required.

The maintainer earlier ruled out a blanket tag-size cap (it would refuse
large Segments and Clusters). These limits are per element
and bound only what the importer must hold whole: a `CodecID` and `DocType`
are short ASCII strings, and `CodecPrivate` is a decoder config (avcC, hvcC,
an Opus or Vorbis header). Pick each limit from its spec and the largest
real decoder config, check them against the MKV fixtures, and return the
importer's existing error for a refused element. Test each element declared
past its limit, fed in small chunks.

Public API: none. Wire: none.

## Related

- [fMP4 import streams samples through open and commit](/quest/m1/security-audit/fmp4-open-commit.md) - the fMP4 leftover of the same streaming work
