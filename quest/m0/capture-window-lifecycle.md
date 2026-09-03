# [S] moq-video: a minimized or resizing window ends capture instead of riding it out

## Goal

Minimizing a captured window pauses its broadcast and resumes when it is
restored, rather than ending the publish. Dragging a window's edge produces one
reopen once the drag settles, not one per mouse-move.

## Plan

Two defects in the native window backends, both reachable from an ordinary
mouse gesture.

**Minimize is fatal on Windows.** `capture/window.rs` compares `GetWindowRect`
against the geometry captured at open. A minimized window reports an off-screen
rect, so the size check fails and the stream ends; `capture_loop` reopens, and
`Capture::open` then rejects it with `SourceUnavailable("window has no
capturable area")`, which is terminal. So minimizing a window kills the
broadcast, where macOS ScreenCaptureKit keeps working. `IsIconic` should hold
the capture instead: sleep the frame interval and keep polling until the window
comes back.

**A drag-resize storms the reopen path.** Both `capture/window.rs` and
`capture/x11.rs` end the stream the first frame the geometry differs. The
publisher then drops the encoder, publishes a discontinuity, reopens the source,
and builds a fresh encoder, all per mouse-move, and each reopen also republishes
the catalog rendition dimensions. Settle the geometry before ending the stream:
once a change is seen, keep polling at the frame interval and only end once the
size has held for a beat. That also stops the encoder being rebuilt for a size
that is about to change again.

Both need a Windows host to verify the Windows half; `just rs windows` must run
on Windows and PR CI is Linux-only.

## Related

- [#2799](/quest/m0/2799-moq-video-capture-negotiates-twice-so-a-window-resize.md) - the other half of the resize story, on the probe-versus-subscriber path
- [Windows window capture](/quest/m0/windows-window-capture-blank.md) - the other defect in the same backend
