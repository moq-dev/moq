# [S] moq-video: X11 and Windows capture rebuild their frame buffers every tick

## Goal

Neither native screen-capture backend allocates a full-frame buffer per frame.
Steady-state capture reuses the same scratch memory and GDI objects.

## Plan

Both backends added in the native screen-capture work allocate the whole frame,
every frame, on the pump thread.

`capture/window.rs`'s `snapshot` creates a memory DC, a compatible bitmap, and a
`vec![0u8; w * h * 4]` per call, then destroys them. At 1080p60 that is roughly
500 MB/s of allocation plus GDI object churn for pixels whose size never changes
while the stream is open. All three belong in `Capture`, built once at open: the
pump thread owns the struct, so the `!Send` handles are fine where they are.

`capture/x11.rs`'s `PixelFormat::rgb` allocates a `w * h * 3` `Vec` and fills it
with three `push` calls per pixel, and `I420::from_rgb` then walks it again into
a third buffer. Take an `&mut Vec<u8>` the `Capture` owns and `clear()` it, so
the allocation happens once.

Neither is a correctness bug, so this is a steady-state cost question: measure
before and after rather than assuming. The X11 half compiles on Linux CI; the
Windows half needs a Windows host (`just rs windows`).

## Related

- [X11 capture transport](/quest/m2/x11-capture-shm.md) - the larger X11 cost, in the same read path
