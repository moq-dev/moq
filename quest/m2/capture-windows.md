# [M] Windows capture parity

## Goal

App capture and system audio work on Windows. Display and window capture,
including the cursor, move to
[Windows.Graphics.Capture](/quest/m2/capture-wgc.md).

## Plan

- **Applications** (every window of a process, including ones that open later)
  are still `Unsupported` outside macOS, where `SCShareableContent` gives it
  almost free. Windows has no direct equivalent, so this needs per-window
  composition or an explicit decision that Windows offers window capture only.
  Decide that rather than leaving a variant that errors at runtime.
- **System audio** needs WASAPI loopback. `moq_audio::capture::Source::System`
  exists but is macOS-only and returns `Unsupported` elsewhere. Unlike macOS
  this does not go through the screen-capture API, so it is an independent path
  and does not inherit the Screen Recording permission coupling.

WGC does not answer app capture. It captures one monitor or one window per
item and has no per-process source, so the per-window-composition or
window-only decision still stands after that backend lands.

## Related

- [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) - display and window capture with the cursor, which this quest no longer owns
- [Linux capture parity](/quest/m2/capture-linux.md) - the same gaps, through
  the portal and PipeWire
