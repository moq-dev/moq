# [M] Fix JS hang catalog/CMAF, watch video rotation, and room metadata bugs

## Goal

`@moq/hang` preserves unknown container/projection values on encode, parses
stats/echo maps safely without catalog version filtering, and decodes optional
`ES_Descriptor` flags in CMAF; `@moq/watch` accounts for rotation when picking
video renditions; and `@moq/room` validates `catalog.hang` and memoizes track
signals against unrelated catalog updates.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful JS media correctness
bugs in `m1` (rejected: `m0` with `release` backports; `m3`).

1. **`js/hang/src/catalog/root.ts:169-181`**:
   - Unwrap unknown `container` and `projection` `.raw` values in
     `Catalog.encode()` for round-trip parity with `rs/hang`.
2. **`js/hang/src/stats.ts:47` & `js/hang/src/echo.ts:56`**:
   - Replace the catalog `renditions()` helper with a plain `__proto__`-safe
     map schema that does not apply media catalog version filtering or insert
     `SKIPPED` entries.
3. **`js/hang/src/cmaf/decode.ts:300-301`**:
   - Parse optional `ES_Descriptor` flags (`0x80` streamDependenceFlag, `0x40`
     URL_Flag, `0x20` OCRstreamFlag) in `extractAudioSpecificConfig` instead of
     assuming all flags are zero.
4. **`js/watch/src/video/source.ts:118-151,207-220,327,348-350`**:
   - Rotate `(codedWidth, codedHeight)` by `section.rotation` (90° / 270°) when
     scoring renditions in `byDimensions()` so portrait/rotated streams pick
     the right rendition.
5. **`js/room/src/metadata.ts:225-249`**:
   - Validate `catalog.hang` with Zod and memoize `userTrack` / `previewTrack`
     signals by structural equality so unrelated catalog updates do not wipe
     and resubscribe active metadata tracks.

Public API: none. Wire: none. Add unit tests in `js/hang`, `js/watch`, and
`js/room`.
