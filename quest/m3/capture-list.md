# [XL] A capture listing a picker can draw

## Goal

`moq_video::capture` lists what can be captured with enough for a picker to
show pictures instead of names, on macOS, Windows, and X11:

- **Apps:** every listed window names its app by an identity that is the same
  after the app restarts, and apps are listed with that identity, a name, and
  their icon as pixels.
- **Placement:** every listed window says which listed display it is on and
  its rectangle there, in that display's pixels.
- **Cameras:** each says what kind of device it is (built in, webcam, capture
  card, phone, virtual) where the system says, and says nothing where it does
  not.
- **Pictures:** a caller gets a current, small picture of any listed window,
  display, or camera at a low rate without holding a full-size capture of it,
  and the docs state what that costs on each desktop.
- **Changes:** a caller waits for the list to change (a camera or monitor
  plugged in or out, a window opened, closed, or retitled) instead of
  re-listing on a timer, and an id names the same item before and after.

Each desktop's path for each of these is decided at setup. What a desktop
cannot do is an `Unsupported` error naming the reason, never an empty list, a
blank picture, or a watch that stays silent. On a Wayland session the caller
learns up front that the system's picker owns windows and displays, so every
window and display listing call is `Unsupported`, even with XWayland running.

Not part of this: capturing an app off macOS, which
[Windows](/quest/m3/capture-windows.md) and
[Linux](/quest/m3/capture-linux.md) capture parity decide, microphones, and
the picker itself.

## Plan

Planned 2026-11-09 (maintainer, studio /quest-plan): ✅ m3, since a named
consumer waits on it (the studio's m0). Consumer: the studio's
[Add page](https://github.com/moq-dev/studio/blob/main/quest/m0/ux/add.md),
through
[its pin](https://github.com/moq-dev/studio/blob/main/quest/m0/pin-capture-list.md).
It draws every window as a live thumbnail under its app's icon, grouped by
app, and each display and camera as a live picture with a device icon, and
refreshes when a device is plugged in or a window opens.

Today (`rs/moq-video/src/capture/mod.rs`) the listing is `Camera { id, name }`,
`Display { id, name, width, height }`, `Window { id, title, app, width,
height }`, and `App { id, name }`, each from a one-shot `cameras()`,
`displays()`, `windows()`, or `apps()`, and `apps()` is `Unsupported` off
macOS. `moq devices` and moq-cli's completions are the other callers and move
with the API.

Settled in the interview:

- Window placement joins the listing (2026-11-10, maintainer, /quest-complete
  follow-up of [moq-dev/moq#8422](https://github.com/moq-dev/moq/pull/8422)):
  ✅ plan it (rejected: a full-screen flag, keeping an excluded app's sound
  out of system audio capture). The studio's privacy shield draws a lock
  where an excluded window was, and its camera that keeps out of the way
  avoids the foreground window; both need a window's position on its
  display. Full screen follows from a window's rectangle against its
  display's, so it gets no field of its own.

- Identity and icons cover every desktop, not macOS alone.
- A supported path per desktop is decided at setup, with an error otherwise
  and never a silent fallback.
- Where the system owns the picker, listing is unsupported and says why.
  xdg-desktop-portal's ScreenCast shows its own picker and hands back a
  stream: no call lists windows or displays or names one by an id that
  outlives the session. [Linux capture parity](/quest/m3/capture-linux.md)
  owns the picked source and its restore token.

What today's code gets wrong for this, found while planning:

- **Identity.** Windows names an app by its process's file stem
  (`app_name` in `capture/wgc/native.rs`), so every packaged app hosted by
  `ApplicationFrameHost` is one app. macOS dedupes `apps()` on the bundle
  identifier, which is empty for an app with no bundle, so those collapse
  into one. X11 has only the `WM_CLASS` class. The identity must survive a
  restart, since the studio keeps lists of apps per user, so a process id is
  not one.
- **Ids across a change.** A display's id is its enumeration index on macOS,
  X11 (`x11:<index>`), and Windows, so plugging in a monitor renumbers the
  rest. An id has to mean one item for as long as it is listed.
- **Listing an app is not capturing it.** `App::source()` builds
  `Source::App`, which is `Unsupported` off macOS, and `moq devices` prints
  app ids for `--app`. An app listed where it cannot be captured must not
  hand out a source that fails at open or an id the CLI refuses.
- **Wayland with XWayland.** `windows()` and `displays()` succeed there with
  XWayland's items only, which a picker would take for the whole desktop.

Recommendations, for the PR to confirm against each system rather than take
as given:

- **Shape.** One handle the caller awaits, yielding the whole current list
  each time it changes, rather than events to apply to a list fetched
  separately. Icons are fetched per app identity at a requested size, apart
  from the list, so a retitled window re-reads no icon.
- **Icons.** The system's own icon for the app: the bundle's on macOS, the
  executable's or package's on Windows, and the window's `_NET_WM_ICON` on
  X11, which is already pixels. Resolving a desktop entry through the icon
  theme would add PNG and SVG decoders; take them only if too many apps set
  no window icon. An app with no icon has none, never a made-up one.
- **Camera kind.** From what the device reports, never from its name:
  AVFoundation's device type, the V4L2 driver or PipeWire node properties,
  and the Media Foundation device attributes. No system tells a capture card
  from a webcam by class, since both are UVC, so expect that kind only where
  a property says so.
- **Pictures.** `capture::open` already takes a size and a rate as hints.
  Reuse it if the hints can be made to hold and the frames reach the caller
  as small CPU pixels; add a call only where they cannot. A camera's picture
  opens the camera, so its light comes on and a device another capture holds
  may refuse; listing alone opens no device. Measure what one picture stream
  costs per item at its default rate on each desktop, and state it on the
  API.
- **Placement.** A window moving or resizing is a change like a retitle, so
  the watch's list stays current; coalesce a drag so it does not wake the
  caller per frame. X11 has `ConfigureNotify`, Windows `EVENT_OBJECT_LOCATIONCHANGE`,
  and macOS reports bounds in each listing, noticed the same way as the
  other window changes.
- **Changes.** X11 has `PropertyNotify` on the root's client list and each
  window's title, and RandR for monitors, which `capture/x11.rs` already
  selects. Cameras have udev and PipeWire's registry on Linux, device
  notifications on Windows, and AVFoundation's connect and disconnect
  notifications on macOS. Windows has `SetWinEventHook`. macOS reports
  another app's windows only through the Accessibility API, so the caller
  picks a macOS mode when it opens the watch, as decided below.
- **macOS window changes.** Decided 2026-11-10 (maintainer,
  /quest-unblock): ✅ both modes, the application picks one at setup
  (rejected: re-list only; Accessibility only; app launch, quit, and
  activation only). In the Accessibility mode moq-video observes each app's
  window events, and setup refuses with `Unsupported` naming the missing
  permission rather than falling back. In the basic mode, which needs no
  Accessibility permission, moq-video re-lists windows at a documented rate only while a
  caller holds a watch, and wakes it on a difference; no caller polls.
  Re-listing is wasteful, so it never runs without a watch. Window pictures
  still need Screen Recording permission in either mode, as any
  ScreenCaptureKit capture does. The Mac tests
  cover both modes.
  [The foreground app](/quest/m4/capture-foreground.md) takes the same mode,
  so one permission serves both.

Tests, sized to what each runner has:

- **Linux under Xvfb**, in the lane the X11 capture workflow runs in
  (`.github/workflows/x11.yml`, Nightly and the release gate): two windows of
  one class with an icon are listed under one app identity with that icon's
  pixels; a window painted one color yields a picture of that color at the
  bounded size; opening, retitling, and closing a window each wake the watch
  with the new list, with no timer in the test.
- **Merge gate, every host:** the identity, id, and list-change logic that
  needs no desktop, and the Wayland refusal.
- **Nightly, Linux with `vivid`** (the lane `just rs v4l2` runs in): loading
  and unloading the module wakes the watch, the camera's kind is virtual, and
  its picture arrives.
- **By hand on a Mac and a Windows desktop**, as ignored tests with each
  result in the PR, since no hosted runner has a desktop session or can
  grant Screen Recording: grouping and icons for a bundled, an unbundled, and
  a packaged app, a picture of each kind of source, and every change in the
  Goal.

Public API: additive for `capture::{Camera, Display, Window, App}`, which are
fenced, and breaking for the listing calls. Wire: none.

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ this quest
stays after the cut, and the m2 fence made the listing structs `#[non_exhaustive]` (rejected: pulling
this quest into m2; a second moq-video break). The fields this adds are then
additive.

## Related

- [macOS capture listing futures are Send](/quest/m3/capture-shareable-send.md) - the ScreenCaptureKit listing this rewrites; whatever replaces it stays `Send`
- [Windows capture parity](/quest/m3/capture-windows.md) - decides whether a listed app can be captured on Windows
- [Linux capture parity](/quest/m3/capture-linux.md) - the portal-picked source that stands in for a list on Wayland, and app capture on Linux
