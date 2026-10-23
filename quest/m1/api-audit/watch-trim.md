# [S] Watch and publish trim

## Goal

`@moq/watch` and `@moq/publish` components report `out.error` with one
consistent type across sources, decoders, and the renderer.

## Plan

Decided 2026-10-22 (API audit). The rest of the trim landed: `Clock`
re-exports, video `arrivals()`, `Player.error`, `Stats.underruns`, read-only
`Publish.Broadcast.net`, and the small items.

Open: the decoders, `Broadcast`, and publish sources and encoders use
`Error | undefined`, while `Video.Source` (`"unsupported"`, on release) and the
`@moq/video` renderer (`"unsupported" | "surface-lost"`) use string codes the
watch UI branches on. Pick one shape. The recommendation is `Error` everywhere,
with the string-code cases becoming `DOMException`s named `NotSupportedError`
and `InvalidStateError`.

Public API: `@moq/watch`, `@moq/publish`, `@moq/video`. Wire: none.
