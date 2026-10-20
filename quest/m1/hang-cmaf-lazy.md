# [M] Load CMAF decode on demand

## Goal

A player that never meets a CMAF rendition never downloads the fMP4 parser.
`@moq/watch` fetches `@moq/hang/cmaf` (still built on
`@svta/cml-iso-bmff`) only when a rendition's container is `cmaf`, and
`@moq/hang` no longer exports a CMAF encoder.

The decode path is about 12 KB raw and 4.3 KB gzip of watch's first load
(measured 2026-10-20: 8 KB of cml and 4 KB of `container/cmaf`). Non-goal: replacing
cml. The parser itself does not change, and `@svta/cml-iso-bmff` stays a
dependency of the published `@moq/hang`. The boundary is the import graph:
nothing reachable from `@moq/hang`'s other exports or watch's static imports
pulls in `@moq/hang/cmaf`.

## Plan

Decided with the maintainer on 2026-10-20:

- Keep cml. A mediabunny spike parsed init segments and fragments correctly,
  but it was 37 KB gzip even tree-shaken, async-only, and built one `Input`
  per frame. It also ignored moq-mux's `depends_on` keyframe rule and
  silently returned no samples for a truncated `mdat`. A hand-written
  reader was offered and declined.
- Boundary: move `js/hang/src/container/cmaf/` out of
  `@moq/hang/container` into its own synchronous `@moq/hang/cmaf` export.
  `Container.Cmaf` goes away. Watch's video and audio decoders
  `await import("@moq/hang/cmaf")` only in their CMAF paths: `#runCmaf`,
  `#runCmafDecoder` and the `supported()` probes, which are already async.
  Downstream bundlers split the chunk. Async wrappers inside hang were
  rejected because every caller turns async anyway.
- Delete the public encoder (`createVideoInitSegment`,
  `createAudioInitSegment`, `encodeDataSegment`, `encode.ts`). Only tests
  and the nightly browser benchmark call it, and publish never writes CMAF.
  `bench/browser/page/cmaf.ts` keeps its decode phase on fixture input and
  drops its encode phase. Tests read the
  fixtures in `rs/moq-mux/src/container/fmp4/test_data/`, so both languages
  check the same files, plus a few checked-in fragments. Consumer tests that
  only exercise jitter, ordering and skips switch to the Legacy format.
- Breaking for `@moq/hang` and maybe `@moq/watch`: add `## Unreleased`
  entries to their `MIGRATE.md`. Point `js/hang/README.md` and
  `doc/lib/js/hang.md`, which still show `Container.Cmaf` and the encoder,
  at `@moq/hang/cmaf`.
- Verify the split with `sh/size/bundles.ts`. The watch first load drops the
  cml and `cmaf` bytes, and a CMAF-only chunk appears behind a dynamic
  import.

## Related

- [fMP4 import unwraps to Legacy](/quest/m1/fmp4-import-unwrap.md) - makes CMAF renditions rare, which is why loading on demand pays
- [Artifact size audit](/quest/m0/audit-size.md) - tracks the watch first-load baseline this cuts
