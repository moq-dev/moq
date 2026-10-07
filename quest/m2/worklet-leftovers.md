# [XS] Drop the leftover worklet types

## Goal

`@moq/hang` no longer depends on `@types/audioworklet`, and `js/moq-boy` no longer includes `js/common/worklet.d.ts`. Packages that still load a worklet keep their types. The shared declaration stays.

## Plan

Decided while completing #4848 (2026-10-05):

- ✅ Remove only those two leftovers. Watch, room, publish, and the audio-quality client keep `@types/audioworklet` and their includes. `js/common/worklet.d.ts` stays.
- Rejected: drop the hang dependency and leave moq-boy's include.
- Rejected: delete the shared declaration and update every consumer.

`js/hang` has no source that names `AudioWorklet` or `?worklet` after #4848. Remove that devDependency.

moq-boy imports `@moq/watch`, whose entry re-exports `./audio`, and `decoder.ts` imports `./render-worklet.ts?worklet`. moq-boy's `tsc` follows that source. The shared include is the only `*?worklet` declaration in that program, so removing it breaks the watch import, not the test mock. A `declare module` in `game.test.ts` cannot cover `decoder.ts`, because that file is already a module.

✅ Add `js/moq-boy/src/worklet.d.ts` with `declare module "*?worklet"`. `include: ["src"]` already picks it up. Then drop the shared include. Require `tsc --noEmit` and `tsc -p tsconfig.build.json` in moq-boy to pass. Do not put the shared include back.

Confirm with `just check` for the packages touched.

Public API: none. Wire: none.
