# [L] Windows capture parity

## Goal

Window capture, app capture, system audio, and cursor capture work on Windows.
Each landed on macOS only.

## Plan

`capture::Source::{Window, App}` are documented "macOS only", and
`moq_audio::capture::Source::System` likewise. Windows needs a different API
for each:

- **Windows and apps** need `Windows.Graphics.Capture`. Desktop Duplication is
  whole-monitor by construction, so this is a second capture backend beside
  the existing one rather than a flag on it. App capture (every window of a
  process, including ones that open later) has no direct equivalent the way
  `SCShareableContent` gives it on macOS, so it needs per-window composition
  or an explicit decision that Windows offers window capture only.
- **System audio** needs WASAPI loopback. Unlike macOS, this does not go
  through the screen-capture API, so it is an independent path and does not
  inherit the Screen Recording permission coupling.
- **The cursor** is not captured at all today: the Desktop Duplication backend
  handles neither `PointerShape` nor `PointerPosition`, so `config.cursor` has
  nothing to control. macOS and Linux both honor it.

Also surface whatever these enumerate through `moq devices`, so a window or
app id is discoverable rather than something the caller must already know.

## Related

- [Linux capture parity](/quest/m2/capture-linux.md) - the same gaps, through
  the portal and PipeWire
