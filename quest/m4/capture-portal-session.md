# [M] Capture from the caller's portal session

## Goal

`moq_video::capture` can open a screen capture on a portal session the caller
already holds: a PipeWire fd and node id from an XDG RemoteDesktop session
that also started its screencast. One consent dialog then covers both seeing
and controlling the screen, and the caller's input lands in the same
coordinate space as the frames.

## Plan

Planned 2026-11-13 (maintainer, /quest-plan of a remote desktop for
kixel.ai). The consumer is Kixel's Wayland desktop host, which injects
keyboard and mouse through libei on a RemoteDesktop session
(`ConnectToEIS`). A ScreenCast session opened separately by moq-video would
ask the user a second time and could pick another monitor than the one
being controlled.

Milestone: ✅ m4 (rejected: m3). The consumer is Kixel's later (m2) plans.

Today `capture/pipewire.rs` opens its own ScreenCast session through ashpd,
with only the `screencast` and `camera` features enabled, and keeps the
restore token itself.

Recommendations for the PR:

- Take the fd and node (and the stream's size and position from the portal)
  rather than an ashpd session type, so moq-video does not choose the
  caller's portal library and the input side stays out of moq.
- The caller owns the session's lifetime and restore token. A capture on a
  session that ends reports the end like any other stopped capture.
- Test against a headless compositor in CI if one runs the RemoteDesktop
  portal; otherwise a by-hand run on GNOME and KDE, recorded in the PR.

Public API: additive. Wire: none.

## Hosts

- linux - the XDG portal and PipeWire

## Related

- [Linux capture parity](/quest/m3/capture-linux.md) - the rest of Wayland capture
