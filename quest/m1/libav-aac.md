# [M] Node and Bun players decode AAC

## Goal

`@moq/hang`'s WebCodecs fallback decodes AAC as well as Opus, so a player
running under Node or Bun (no native `AudioDecoder`) plays an AAC broadcast
instead of failing. Today `js/hang/src/util/libav.ts` loads the Opus-only
`@libav.js/variant-opus-af` and carries a `TODO build with AAC support`.

Once it lands, the Rust CLI interop publisher adds an AAC track beside its
Opus one, so every interop subscriber decodes AAC through the
symphonia-backed native paths and this fallback.

## Plan

Decided in planning (2026-10-20): found while planning [interop audio
delivery](/quest/m1/interop-audio.md), which publishes Opus from Rust until
this lands. Whichever of the two lands second adds the Rust AAC track and its
tone check to the native JS cells.

Recommendations:

- Prefer a published libav.js variant that includes an AAC decoder over a
  custom build; check its license and size, since the fallback ships in the
  package. If only a custom build works, say what it costs.
- Load the larger variant only when a broadcast needs AAC, so Opus-only
  players do not pay for it.
- Land a regression test that decodes AAC under Bun without `AudioDecoder`
  and fails on today's Opus-only fallback.

Public API: none expected. Wire: none.

## Related

- [Interop audio delivery](/quest/m1/interop-audio.md) - the matrix this completes for AAC
- [Native audio decode conformance](/quest/m1/audio-decode-conformance.md) - the Rust side of AAC decode
