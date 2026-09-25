# [M] Experimental WebGPU renderer

## Goal

Implement and verify the behavior tracked in [#703](https://github.com/moq-dev/moq/issues/703)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

**WARNING** This is a pre-mature optimization or gimmick at best.

We currently use [Canvas2D](https://github.com/kixelated/moq/blob/ff7cf92679e16c1d18ca5862aa4c7f73417e3c36/js/hang/src/watch/video/renderer.ts#L97) to render individual frames. This is pretty basic but works.

All major browsers support WebGPU now. It has a [copyExternalImageToTexture](https://developer.mozilla.org/en-US/docs/Web/API/GPUQueue/copyExternalImageToTexture) method that apparently copies a `VideoFrame` to a renderable texture. This apparently avoids a copy so it might be faster than Canvas2D but probably not.

The main benefit of WebGPU is being able to do *other* stuff, like run AI models on pixel data without copying to the CPU. Or rendering a person's face on a teapot. Or using shaders for gimmicky effects. None of this is really generic enough for a MoQ library but maybe somebody wants to have some fun.

## Closes

- [#703](https://github.com/moq-dev/moq/issues/703) - close this issue when the quest finishes
