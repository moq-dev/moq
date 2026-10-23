# [L] Linux capture parity

## Goal

Window capture and system audio work on a Wayland desktop, and app capture has
a stated answer rather than a runtime error.

## Plan

X11 answered half of this already: `capture/x11.rs` enumerates monitors and
windows with stable `x11:<id>` selectors, captures either natively, composites
the XFixes cursor, and is the default when no Wayland session is running. So
scripted capture of a chosen monitor *is* expressible on X11, and XWayland
windows are reachable by explicit id even under Wayland.

The portal path is what is left, and it is different in kind rather than merely
unimplemented, because xdg-desktop-portal owns source selection:

- Portal-picked windows reach the CLI: #5089 added
  `capture::Source::Portal(portal::Selection)`, and #5566 sends a bare
  `--window` to the portal (`doc/bin/cli.md`).
- **Display selection stays picker-driven.** On Wayland a bare `--display`
  goes to the portal and a non-X11 id is refused. Since #5089 a `Selection` replays
  its restore token across reopens within one process, but the token is never
  exposed, so a new `moq` run always shows the picker. Either the token becomes
  persistable (API plus CLI) so a previously-approved source is reused without
  the picker, or this stays a documented limitation answered by the X11 path.
- **System audio** needs a PipeWire monitor node.
  `moq_audio::capture::Source::System` exists but is macOS-only and returns
  `Unsupported` elsewhere. This is independent of the screencast portal.
- **Applications** have no portal concept behind them, so decide explicitly
  whether Linux offers app capture rather than leaving a variant that errors at
  runtime.

## Related

- [Windows capture parity](/quest/m2/capture-windows.md) - the same gaps,
  through Windows.Graphics.Capture and WASAPI
