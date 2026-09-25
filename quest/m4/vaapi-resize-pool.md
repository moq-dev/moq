# [S] Pool VAAPI resize surfaces

## Goal

A VAAPI resize reuses one output surface per destination size instead of allocating one per frame. `Surface::resize` stays the same call. The decoder pool is unchanged.

## Plan

`Processor` in moq-vaapi allocates the blit output with `ExportedFrame::from_surface` on every resize. Keep one surface per output size. When the exported frame drops, that surface returns and the next blit of the same size uses it. A frame the consumer still holds is not overwritten; the processor allocates another. The pool keeps one free surface per size and destroys any surface returned past that, so a burst of released frames cannot park them all. That is the decoder pool's rule, applied to resize outputs.

moq-video already blits through `Processor` and exports the result. The reuse test belongs in moq-vaapi. Here, bump the workspace requirement and confirm a resize still returns an NV12 DMA-BUF.

## Required

- A `moq-vaapi` release whose `Processor` reuses one output surface per destination size and destroys free surfaces past one per size

## Related

- [VAAPI encode and decode](/quest/m4/video-vaapi.md) - H.265 and checked-in bindings, the other moq-vaapi gate
