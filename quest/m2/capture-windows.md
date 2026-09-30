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

Note that the window capture that landed is GDI. It now reaches
DirectComposition content through `PrintWindow(PW_RENDERFULLCONTENT)`, but that
call runs on the target window's UI thread, so the backend probes the window for
responsiveness every frame and skips one that is hung, and it still round-trips
BGRA through the CPU on its way to I420. Windows.Graphics.Capture has neither
coupling and delivers `IDirect3DSurface` frames the existing D3D11 path could
take zero-copy. WGC would also answer app capture and the cursor, so weigh
these three against doing that once.

Decided in the 2026-09-30 audit: the Windows half of the frame-buffer quest
folds in here. `snapshot` in `capture/window.rs` (lines 334-392) creates a
memory DC, a compatible bitmap, and a `vec![0u8; w * h * 4]` per call, then
destroys them: roughly 500 MB/s of allocation plus GDI object churn at 1080p60.
Build all three once at open in `Capture`; the pump thread owns it, so the
`!Send` handles are fine. Skip this if WGC replaces the GDI path first, and
measure before and after on a Windows host (`just rs windows`).

## Related

- [Linux capture parity](/quest/m2/capture-linux.md) - the same gaps, through
  the portal and PipeWire
