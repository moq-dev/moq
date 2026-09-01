# [L] Windows capture parity

## Goal

App capture, system audio, and whole-screen cursor capture work on Windows.
Window capture landed with the native screen capture work; these are what it
left behind.

## Plan

Window capture and its cursor now exist: `capture/window.rs` enumerates
top-level windows, captures the selected one through GDI, composites the cursor
when `config.cursor` asks for it, and surfaces the ids through `moq devices`.
What remains:

- **Applications** (every window of a process, including ones that open later)
  are still `Unsupported` outside macOS, where `SCShareableContent` gives it
  almost free. Windows has no direct equivalent, so this needs per-window
  composition or an explicit decision that Windows offers window capture only.
  Decide that rather than leaving a variant that errors at runtime.
- **System audio** needs WASAPI loopback. `moq_audio::capture::Source::System`
  exists but is macOS-only and returns `Unsupported` elsewhere. Unlike macOS
  this does not go through the screen-capture API, so it is an independent path
  and does not inherit the Screen Recording permission coupling.
- **The cursor on whole-screen capture** is still missing: the Desktop
  Duplication backend handles neither `PointerShape` nor `PointerPosition`, so
  `config.cursor` controls nothing there. Window capture and both other
  platforms honor it, which makes this the odd one out.

Note that the window capture that landed is GDI, which is why
[the black-frame quest](/quest/m0/windows-window-capture-blank.md) exists: it
reaches nothing rendered through DirectComposition. If that quest moves to
Windows.Graphics.Capture, WGC also answers app capture and the cursor, so weigh
these three against doing that once.

## Related

- [Linux capture parity](/quest/m2/capture-linux.md) - the same gaps, through
  the portal and PipeWire
- [Windows window capture](/quest/m0/windows-window-capture-blank.md) - the GDI
  backend's blank output, and the WGC decision that would subsume much of this
