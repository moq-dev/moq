# [S] One policy for oversized container metadata

## Goal

The fMP4 and MKV importers follow one documented policy for container metadata that declares a huge size: the same rule for when it is refused, the same bound or the same reason for having none, and the same error shape, so a caller (HLS ingest, `moq import`, the bindings) sees one behavior whichever container it feeds.

## Plan

The two importers settled this separately during the 2026-10-27 security audit:

- fMP4 ([moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232), merged): what must be held whole, a `moov` or `moof` body or one text sample, is refused at its header when it declares more than `fmp4::MAX_HELD_BYTES` (32 MiB, the transport's per-group `MAX_CACHE_BYTES`), and a `moof` declaring more than `fmp4::MAX_FRAGMENT_SAMPLES` (2^21) is refused when it parses. That reversed the 2026-10-28 incremental plan, because incremental parsing could not bound what it kept.
- MKV ([moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234), merged): metadata parses incrementally with no blanket size cap (maintainer, 2026-10-28). Each collected master keeps one value per field, so repeats replace rather than accumulate. [moq-dev/moq#7629](https://github.com/moq-dev/moq/pull/7629) bounded the fields read whole: `DocType` and `CodecID` by the longest supported value, and `CodecPrivate` by each codec's supported configuration, refused at its header (FLAC STREAMINFO alone, the RFC 7845 OpusHead, av1C with a sequence header and HDR metadata, avcC by its count fields at about 34 MiB, and an AudioSpecificConfig by its syntax maximum). An hvcC is canonicalized as it arrives instead: its parameter sets are kept, at most one per id, and SEI is discarded unread. These are private constants in `rs/moq-mux/src/container/mkv/import.rs`, `codec::h264::AVCC_MAX_LEN`, and `codec::aac::CONFIG_MAX_LEN`.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. Pick one policy for both importers, rather than leaving each at the decision its own audit PR made. Starting point: a byte bound at the header for whatever must be held whole, since #7232 found that is what actually bounds memory, while MKV keeps streaming what it can consume incrementally. Bring the choice to the maintainer in the PR if the evidence points elsewhere.

- #7232 has landed the fMP4 side (`MAX_HELD_BYTES` and `MAX_FRAGMENT_SAMPLES` in `rs/moq-mux/src/container/fmp4/mod.rs`). Note the MKV avcC bound (about 34 MiB) sits above fMP4's 32 MiB `MAX_HELD_BYTES`.
- Inventory what each importer still holds whole, and how much parsed state it builds from it (#7232 noted a 32 MiB `moof` decodes into larger structures).
- Keep audio and video payloads unbounded in both: streaming each sample into its frame was decided 2026-10-27, and this quest is about metadata only. An fMP4 text sample is the one payload held whole, because its rewrite needs every byte, so `MAX_HELD_BYTES` covers it; keep that exception. MKV has no counterpart, since it refuses subtitle tracks.
- Document the policy once, in moq-mux's import docs, and make both importers' bounds `pub const`s as #7232's are.

Public API: possibly new `pub const` bounds and a shared refusal error in moq-mux. Wire: none.

## Related

- [fMP4 sample plans, trun counts, and trak counts are bounded](/quest/m1/fmp4-metadata-limits.md) - measured what fMP4's bounds let a `moof` or `moov` build, and sets the bounds this policy documents
