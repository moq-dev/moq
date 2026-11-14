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
- **Display selection stays picker-driven in the CLI.** On Wayland a bare
  `--display` goes to the portal and a non-X11 id is refused. Decided
  2026-10-26 (maintainer): the token becomes persistable (API plus CLI), so a
  previously-approved source is reused without the picker. The API half is
  done: [moq-dev/moq#8562](https://github.com/moq-dev/moq/pull/8562) added
  `Selection::restore_token` and `with_restore_token`
  (`rs/moq-video/src/capture/portal.rs`), which a downstream application
  consumes. What remains is the CLI: nothing in `rs/moq-cli` persists or
  seeds the token, so a new `moq` run still shows the picker.
- **System audio** needs a PipeWire monitor node.
  `moq_audio::capture::Source::System` exists but is macOS-only and returns
  `Unsupported` elsewhere. This is independent of the screencast portal.
- **Applications** have no portal concept behind them, so decide explicitly
  whether Linux offers app capture rather than leaving a variant that errors at
  runtime.
- **Per-app audio** (decided 2026-10-26, maintainer: yes) captures one
  application's output from its PipeWire stream node, so a creator can stream
  a game without their music. Apps are listed from the nodes present, so only
  a running app can be chosen. An app is every stream node with its
  `application.process.id` (one game may open several), mixed together, and
  capture follows nodes as the app opens and closes them, matching the
  Windows process tree and the macOS application. The Windows and macOS halves
  share the same `moq_audio::capture` source.

## Waiting

- Maintainer: whether app capture exists off macOS, here and in [Windows capture parity](/quest/m3/capture-windows.md). (A) Linux and Windows offer window capture only: `Source::App` stays macOS-only and fails with its reason elsewhere, and a listing off macOS uses apps only to group windows. (B) Build app capture by composing an app's windows. I recommend A, since no consumer asks for B and it matches the capture listing's rule that a listed app must not hand out a source that fails at open.

## Related

- [Windows capture parity](/quest/m3/capture-windows.md) - the same gaps,
  through Windows.Graphics.Capture and WASAPI
- [A capture listing a picker can draw](/quest/m3/capture-list.md) - lists apps on X11 with the identity an app capture would name, and tells a caller that the picker stands in for a list on Wayland
