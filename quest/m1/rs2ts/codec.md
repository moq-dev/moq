# [L] Generated codec

## Goal

js/net's lite codec is generated from moq-net by the pinned rs2ts and
committed. A CI lane regenerates it and fails on drift, and the generated
codec passes `just test interop --all` in place of the hand-written one.
js/net's `U64` comes from the rs2ts runtime package. The PR carries the
line's go/no-go: the generated codec's bundle size and per-frame CPU against
js/net's hand-written codec. A no-go stops the
[line](/quest/m1/rs2ts/README.md) before any moq-net API break.

## Plan

- Only the regeneration lane needs rs2ts's toolchain; it comes from the flake
  input. JS contributors and npm publishing use the committed output.
- How to regenerate goes in js/net's README beside the lane, not in `doc/`.
- The extern bindings (tracing facade hook, `moq_time::Instant` to
  `@moq/time` if the [time line](/quest/m1/time/README.md) has landed) live
  in moq's rs2ts config, not in rs2ts.
- Keep js/net's size budget in view: `lite/*` is 15 KB gzip today. Watch for
  std shims pulling in weight.

Public API: `@moq/net` gains a runtime dependency; the codec's exports keep
their shape. Wire: none.

## Required

- [rs2ts repo](/quest/m1/rs2ts/repo.md) - the pinned translator and runtime
- [Codec refactors](/quest/m1/rs2ts/codec-refactor.md) - the codec extracts on its own
