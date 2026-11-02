# [S] One policy for oversized container metadata

## Goal

The fMP4 and MKV importers follow one documented policy for container metadata that declares a huge size: the same rule for when it is refused, the same bound or the same reason for having none, and the same error shape, so a caller (HLS ingest, `moq import`, the bindings) sees one behavior whichever container it feeds.

## Plan

The two importers settled this separately during the 2026-10-27 security audit:

- fMP4 ([moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232), merged): what must be held whole, a `moov` or `moof` body or one text sample, is refused at its header when it declares more than `fmp4::MAX_HELD_BYTES` (32 MiB, the transport's per-group `MAX_CACHE_BYTES`), and a `moof` declaring more than `fmp4::MAX_FRAGMENT_SAMPLES` (2^21) is refused when it parses. That reversed the 2026-10-28 incremental plan, because incremental parsing could not bound what it kept.
- MKV ([moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234), merged): metadata parses incrementally with no blanket size cap (maintainer, 2026-10-28). Each collected master keeps one value per field, so repeats replace rather than accumulate, but fields read whole (`CodecID`, `DocType`, `CodecPrivate`) have no bound.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. Pick one policy for both importers, rather than leaving each at the decision its own audit PR made. Starting point: a byte bound at the header for whatever must be held whole, since #7232 found that is what actually bounds memory, while MKV keeps streaming what it can consume incrementally. Bring the choice to the maintainer in the PR if the evidence points elsewhere.

- #7232 has landed the fMP4 side (`MAX_HELD_BYTES` and `MAX_FRAGMENT_SAMPLES` in `rs/moq-mux/src/container/fmp4/mod.rs`). Wait for MKV leaf limits (below), since it sets MKV's bounds on the fields read whole.
- Inventory what each importer still holds whole, and how much parsed state it builds from it (#7232 noted a 32 MiB `moof` decodes into larger structures).
- Keep audio and video payloads unbounded in both: streaming each sample into its frame was decided 2026-10-27, and this quest is about metadata only. An fMP4 text sample is the one payload held whole, because its rewrite needs every byte, so `MAX_HELD_BYTES` covers it; keep that exception. MKV has no counterpart, since it refuses subtitle tracks.
- Document the policy once, in moq-mux's import docs, and make both importers' bounds `pub const`s as #7232's are.

The `fmp4` and `mkv` fuzz targets (`rs/moq-mux/fuzz`) bound heap work per input byte (`moq_mux::fuzz::run`), so they measure parsed-state amplification under either policy.

Public API: possibly new `pub const` bounds and a shared refusal error in moq-mux. Wire: none.

## Related

- [MKV leaf limits](https://github.com/moq-dev/moq/pull/7468) - planned in that open PR as `quest/m1/security-audit/mkv-leaf-limits.md`: per-element limits on the three MKV fields read whole, in m1. This quest starts from whatever it lands and aligns the two importers' rules, not the limits it picked
