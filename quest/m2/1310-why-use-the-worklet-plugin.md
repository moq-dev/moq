# [XS] why use the worklet plugin?

## Goal

Implement and verify the behavior tracked in [#1310](https://github.com/moq-dev/moq/issues/1310)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming from a question into a decision quest:
either replace vite-plugin-worklet with vite's ?worker inline imports, or
keep it and document why library builds need the esbuild-inlined blob URL.

### Issue context

i saw that you have a modified version of `vite-plugin-worklet` in the repo, but from my testing it does the same thing as importing the worklet code with [`?worker`](https://vite.dev/guide/assets#importing-script-as-a-worker).

it's also [mentioned at one place](https://github.com/moq-dev/moq/blob/c28a7c10c2111f604fa6f763870732af28979bed/js/hang/src/vite-env.d.ts#L3) but then not used anywhere.

is there a specific reason for inlining the code in base64 over importing it via Worker imports?

i can open a pr replacing the worklet plugin, etc. with worker imports if you'd like

## Closes

- [#1310](https://github.com/moq-dev/moq/issues/1310) - close this issue when the quest finishes
