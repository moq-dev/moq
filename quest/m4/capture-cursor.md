# [L] The cursor apart from the frame

## Goal

A screen capture in `moq_video::capture` can report the cursor separately
instead of drawing it into each frame: its position whenever it moves, and
its image and hotspot whenever the shape changes. A viewer can then draw the
pointer itself, with no round trip of lag. Where a system cannot report the
cursor's shape or position, asking for it is an `Unsupported` error naming
the reason, decided at setup.

## Plan

Planned 2026-11-13 (maintainer, /quest-plan of a remote desktop for
kixel.ai). The consumer is Kixel's desktop page, which draws the remote
pointer locally so a human's aim does not trail by the round-trip time, as
RDP and Parsec do. The track that carries it is Kixel's convention, not
hang's, until a second consumer needs it.

Decided: ✅ a separate cursor (rejected: drawn into the frame, with the
viewer's own pointer shown on top). Milestone: ✅ m4 (rejected: m3), since
the consumer is Kixel's later (m2) plans.

Today `capture::Config::cursor` is a bool that draws the cursor in or leaves
it out: the portal's embedded mode, `SetIsCursorCaptureEnabled` on WGC,
`setShowsCursor` on macOS, and a CPU blend of the XFixes cursor on X11.

What each system offers, to confirm in the PR:

- **Wayland:** the portal's metadata cursor mode puts position and bitmap in
  each PipeWire buffer's metadata.
- **X11:** XFixes reports shape changes and their image; position by query
  or XInput2 motion events.
- **Windows:** DXGI Desktop Duplication gives pointer shape and position, but
  capture moved to WGC, which has neither. `GetCursorInfo` plus the cursor's
  icon may do.
- **macOS:** position from `CGEvent`. No public API gives another app's
  cursor image (`NSCursor.currentSystemCursor` is deprecated). Find out what
  works, and say what does not.

Recommendations: a stream beside the frames, in the capture's own coordinate
space and clock, with each shape sent once and named by id after. The report
is the current state (shape and position) plus change notifications, not an
event log, so a viewer that joins or reconnects reads the current shape before
any position that names it. Whatever carries it should start each shape's group
with that shape, so a late reader at the newest group needs no replay.
On a Wayland session the caller opened, the metadata cursor mode must have
been selected before the session started
([the portal session](/quest/m4/capture-portal-session.md)); another mode is
`Unsupported`.

Public API: additive. Wire: none.

## Hosts

- linux - the portal metadata cursor and XFixes
- macos - the cursor without a public shape API
- windows - the cursor under WGC
