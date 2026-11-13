# [M] Leaving apps out of a display capture

## Goal

A display capture in `moq_video::capture` can name apps to leave out, by the
identity [the capture listing](/quest/m3/capture-list.md) gives them. No
frame it delivers shows a window of an excluded app, including a window that
opens later and an app that starts after the capture did. The set can change
while the capture runs, and an app added to it is gone from the next frame.

Whether a desktop can do this is decided when the capture is set up. Where
the system cannot leave another app's windows out, asking for an exclusion is
an `Unsupported` error naming the reason, never a capture that shows the app
anyway. A caller can ask whether exclusion is supported without opening a
capture.

Not part of this: hiding the caller's own windows. On Windows,
`SetWindowDisplayAffinity` does that for the caller's own windows. On macOS,
ScreenCaptureKit ignores `NSWindow.sharingType`, so the caller names its own
app in the exclusion like any other; elsewhere its windows are not protected.
Also not part of this: drawing anything where an excluded window was,
since the capture shows whatever the system composites behind it; window and
app captures, which already show only what was chosen; and sound, since
`moq_audio`'s system capture still carries an excluded app's audio.

## Plan

Planned 2026-11-09 (maintainer, studio /quest-plan). Consumer: the studio's
[privacy shield](https://github.com/moq-dev/studio/blob/main/quest/m1/privacy-shield.md),
through
[its pin](https://github.com/moq-dev/studio/blob/main/quest/m1/pin-capture-exclude.md).
It keeps apps the streamer marks private, such as a password manager, out of
every display capture, and turns the setting off with a note on a desktop
that cannot protect them.

Milestone: ✅ m4 (rejected: m3). The maintainer's rule in that interview is
that work a named consumer waits on joins m3, and
[m3](/quest/m3/README.md) sends work only a downstream's later plans need to
m4. The consumer is in the studio's m1, "after the studio ships".

What each desktop can do, found while planning. Re-check each before relying
on it, and record what the PR finds here:

- **macOS: supported.** `open_display` in `capture/screencapture.rs` already
  builds its `SCContentFilter` with an empty list of excluded windows, and
  ScreenCaptureKit also filters by app and updates a running stream's
  filter.
- **Windows: unsupported.** Windows.Graphics.Capture captures a monitor
  whole, with no filter. `SetWindowDisplayAffinity` leaves a window out of
  capture only when the caller's own process owns it, which is the caller's
  half above. The Magnification API has an exclusion list, but the callback
  that hands back its pixels is deprecated since Windows 7, runs only with
  the Desktop Window Manager off, and has no replacement.
- **X11: unsupported.** A display capture reads the root window, which
  already holds every window. Neither the core protocol nor Composite leaves
  one out short of moq-video compositing the desktop itself.
- **Wayland: unsupported.** The portal's picker chooses what is shared and
  takes no exclusion.

Rejected: painting over an excluded window's rectangle after capture on the
desktops that cannot filter. The rectangle is read apart from the frame, so
a moving or newly opened window shows for a frame or more, which claims a
protection it does not give.

Recommendations for the PR:

- **Apps, not single windows.** The consumer marks apps, and nothing asks to
  leave out one window of an app that otherwise shows.
- **A late app.** A filter that excludes running apps cannot name one that
  has not started, so a private app could show between its launch and the
  filter's update. Either prove that gap cannot reach a frame or build the
  filter from the apps to include, so an app nobody has allowed is absent
  until it is added. An inclusion filter must admit each app that is not
  excluded as it launches, so excluding one app never hides a newly started
  game or browser. The Goal holds either way.
- **Shape.** The exclusion belongs to the display selection, not to a
  `Config` field every other source ignores, as `cursor` is by cameras. It
  is a breaking change to `Source::Display` if it lands there.

Tests:

- **Merge gate, every host:** each backend that cannot filter (X11, WGC, the
  portal) refuses a display capture with an exclusion at setup, and the
  support query says the same without opening anything.
- **By hand on a Mac**, as ignored tests with each result in the PR, since
  no hosted runner can grant Screen Recording: a window of a known color is
  on screen and no captured frame holds that color once its app is excluded;
  the same for a second window the app opens later, for an app started after
  the capture, and for an app added to the set mid-capture.

Public API: breaking if the exclusion joins `Source::Display`, additive
otherwise. Wire: none.

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ this quest
stays after the cut, and
[capture listings and selections can grow](/quest/m2/api-capture-fences.md)
fences the display selection the exclusion joins in m2 (rejected: pulling this quest into m2; a second moq-video
break). What this adds is then additive.

## Required

- [Capture listings and selections can grow](/quest/m2/api-capture-fences.md) - the fenced structs this adds fields to
- [A capture listing a picker can draw](/quest/m3/capture-list.md) - the app identity an exclusion names

## Related

- [The foreground app](/quest/m4/capture-foreground.md) - names apps by the same identity
- [Per-app audio on macOS](/quest/m4/capture-app-audio-macos.md) - the same content filter, limiting audio to one app
