# [S] moq-video: X11 capture rebuilds its frame buffers every tick

## Goal

X11 screen capture does not allocate a full-frame buffer per frame.
Steady-state capture reuses the same scratch memory.

## Plan

The X11 backend added in the native screen-capture work allocates the whole
frame, every frame, on the pump thread. The Windows GDI path had the same cost
and is deleted by [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) instead.

`capture/x11.rs`'s `PixelFormat::rgb` allocates a `w * h * 3` `Vec` and fills it
with three `push` calls per pixel, and `I420::from_rgb` then walks it again into
a third buffer. Take an `&mut Vec<u8>` the `Capture` owns and `clear()` it, so
the allocation happens once.

This is not a correctness bug, so it is a steady-state cost question: measure
before and after rather than assuming.

## Related

- [X11 capture transport](/quest/m2/x11-capture-shm.md) - the larger X11 cost, in the same read path
