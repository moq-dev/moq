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

- Take the stream the portal describes (node id and the stream's properties)
  rather than an ashpd session type, so moq-video does not choose the
  caller's portal library and the input side stays out of moq. Forward the
  stream's `mapping_id` unchanged, since it is what matches the stream to the
  caller's libei region, and take `position` and `size` as optional: the
  portal sends `position` only for monitor streams and `size` is optional, so
  a window stream has no position. Frame pixel size comes from the negotiated
  PipeWire format, never from the portal's compositor-space `size`, and a
  missing value is reported to the caller as absent, not guessed.
- Take a handle that yields a fresh PipeWire remote fd for each open (the
  caller's `OpenPipeWireRemote` on its session), not one fd. The PipeWire
  connection consumes its fd, and capture reopens after a format change and
  for each on-demand viewer, so a single fd serves only the first open. A
  one-method trait the caller implements fits better than a callback
  parameter; settle its shape in the PR.
- The caller selects the metadata cursor mode in `SelectSources` before it
  starts the session, because the portal fixes the cursor mode there. The
  stream description carries the mode the caller chose, and a
  [cursor](/quest/m4/capture-cursor.md) request on a session with another
  mode is `Unsupported` at setup, naming the mode.
- The caller owns the session's lifetime and restore token. A capture on a
  session that ends reports the end like any other stopped capture.
- Test against a headless compositor in CI if one runs the RemoteDesktop
  portal; otherwise a by-hand run on GNOME and KDE, recorded in the PR.

Public API: additive. Wire: none.

## Hosts

- linux - the XDG portal and PipeWire

## Related

- [The cursor apart from the frame](/quest/m4/capture-cursor.md) - needs the metadata cursor mode this session carries
- [Linux capture parity](/quest/m3/capture-linux.md) - the rest of Wayland capture
