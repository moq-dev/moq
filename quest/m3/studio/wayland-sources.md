# [M] Wayland display and window sources

## Goal

On a Wayland desktop, `moq-studio` adds display and window sources through
the xdg-desktop-portal picker. A source the user approved once reopens
without the picker, across restarts of the studio. Today display and window
sources capture only X11 and XWayland.

## Plan

Decided 2026-10-26 (maintainer):

- **Token:** the portal restore token is persisted, which
  [Linux capture parity](/quest/m2/capture-linux.md) makes possible in
  moq-video. This quest consumes it.
- **Storage:** the token lives in per-machine state (the XDG state directory,
  keyed by scene collection and source), never in the scene file, so the scene
  file stays portable and diffable. On another machine, or when the portal
  rejects the token, the picker shows once and the new token is stored.
- **Scene file:** the scene item records that the source is portal-picked, not
  which monitor or window was picked, since only the portal knows that.

Add the source kinds next to the X11 ones in moq-scene and the Sources panel,
and update `doc/bin/studio.md`, which says display and window sources are
X11-only.

## Required

- [Linux capture parity](/quest/m2/capture-linux.md) - the portal restore token becomes persistable
