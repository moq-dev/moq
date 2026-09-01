# [L] Linux capture parity

## Goal

Window capture and system audio work on Linux, and a chosen display can be
captured without a human at the picker.

## Plan

The xdg-desktop-portal owns source selection, which is what makes this
different from the other platforms rather than merely unimplemented:

- **Windows** are not offered at all: the portal is asked for
  `SourceType::Monitor` only, so the picker never shows one. Adding
  `SourceType::Window` is the change, but the portal still owns which window
  the user picks.
- **Display selection is not expressible.** The device selector is explicitly
  ignored, so scripted or headless capture of a chosen monitor cannot be
  written. Either the restore-token flow lets a previously-approved source be
  reused without the picker, or this stays a documented limitation of the
  portal path and the X11/DRM direct path is what answers it.
- **System audio** needs a PipeWire monitor node, which is independent of the
  screencast portal.

App capture (every window of one process) has no portal concept behind it, so
decide explicitly whether Linux offers it rather than leaving it as an
unimplemented variant that errors at runtime.

## Related

- [Windows capture parity](/quest/m2/capture-windows.md) - the same gaps,
  through Windows.Graphics.Capture and WASAPI
