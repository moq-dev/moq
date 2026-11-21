# [S] Fuzz the moq-transport draft-23 codec

## Goal

The IETF fuzz targets in `rs/moq-net/src/fuzz.rs` decode at draft-23, and every stored regression under `rs/moq-net/fuzz/regressions/` still replays the input it was committed for.

## Plan

`IETF_VERSIONS` lists drafts 18, 20, and 22. The targets pick a version by an input byte modulo the list's length, so appending draft-23 re-targets three of the five stored ingest regressions and changes what one settings value means in the ingest target ([moq-dev/moq#8975](https://github.com/moq-dev/moq/pull/8975)). Decided 2026-11-21 (maintainer): ✅ keep the existing selector mapping for drafts 18 to 22 and give draft-23 a value outside it, so stored inputs keep their meaning (rejected: append draft-23 and re-derive the stored regressions at their old versions).

## Related

- [Offer draft-23 by default](/quest/m4/ietf-draft23-default.md) - the port's other remaining work
