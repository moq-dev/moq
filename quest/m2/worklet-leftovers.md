# [XS] Drop the leftover worklet types

## Goal

`@moq/hang` no longer depends on `@types/audioworklet`, and `js/moq-boy` no longer includes `js/common/worklet.d.ts`. Packages that still load a worklet keep their types. The shared declaration stays.

## Plan

Decided while completing #4848 (2026-10-05):

- ✅ Remove only those two leftovers. Watch, room, publish, and the audio-quality client keep `@types/audioworklet` and their includes. `js/common/worklet.d.ts` stays.
- Rejected: drop the hang dependency and leave moq-boy's include.
- Rejected: delete the shared declaration and update every consumer.

`js/hang` has no source that names `AudioWorklet` or `?worklet` after #4848. Remove that devDependency. `js/moq-boy/src/game.test.ts` mocks a `?worklet` URL so Bun can load the game without the audio decoder. If removing the include makes that mock fail to typecheck, declare that one module in the test file. Do not put the package include back.

Confirm with `just check` for the packages touched.

Public API: none. Wire: none.
