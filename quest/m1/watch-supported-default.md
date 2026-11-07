# [XS] Watch sources default to the decoder's probe

## Goal

A composer that builds `@moq/watch`'s `Audio.Source` or `Video.Source`
without `in.supported` gets playback. Today the input defaults to
`undefined`, and "nothing is selected until one is provided"
(`js/watch/src/audio/source.ts`), so forgetting it is silent non-playback.
When `in.supported` is absent, the source uses its decoder's `supported`
probe.

## Plan

Found by the 2026-11-07 API shape audit (browser media). Additive: callers
that pass `supported`, such as the `<moq-watch>` element, are unchanged. Add a test that a source built without it selects a
rendition.
