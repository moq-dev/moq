# [M] The foreground app

## Goal

`moq_video::capture` reports which app and which of its windows are in
front, and a caller waits for that to change instead of asking on a timer.
The app and the window carry the identities
[the capture listing](/quest/m3/capture-list.md) gives them, so a caller
matches them against the list and captures the window it names. No app in
front, such as a bare desktop, is a value, not an error.

It works on macOS, Windows, and X11 under a window manager that publishes
the active window. Where the desktop does not tell an app, which is every
Wayland session, setting it up is an `Unsupported` error naming the reason,
decided before the first report, never a report that stays empty or stale.

## Plan

Planned 2026-11-09 (maintainer, studio /quest-plan). Consumers: the studio's
[source that follows the game](https://github.com/moq-dev/studio/blob/main/quest/m1/follow-game.md),
through
[its pin](https://github.com/moq-dev/studio/blob/main/quest/m1/pin-capture-foreground.md),
which shows whichever app is in front and refuses to be added where the
desktop does not report one, and its
[camera that keeps out of the way](https://github.com/moq-dev/studio/blob/main/quest/m1/auto-place.md),
which treats the foreground window as mattering.

Milestone: ✅ m4 (rejected: m3). The maintainer's rule in that interview is
that work a named consumer waits on joins m3, and
[m3](/quest/m3/README.md) sends work only a downstream's later plans need to
m4. Both consumers are in the studio's m1, "after the studio ships".

Nothing in `rs/moq-video/src/capture/` reports focus today.

Recommendations, for the PR to confirm against each system:

- **Shape.** A handle the caller awaits, yielding the current app and window
  each time either changes, like the listing's.
- **X11.** The root's `_NET_ACTIVE_WINDOW`, watched with `PropertyNotify`.
  Supported only when `_NET_SUPPORTED` lists it, checked at setup.
- **Windows.** `GetForegroundWindow`, watched with `SetWinEventHook`, which
  needs a thread pumping messages. moq-video owns that thread.
- **macOS.** `NSWorkspace` names the frontmost app and posts when it
  changes. Those notifications arrive only while the main run loop runs, so
  setup either checks for one or says what the caller must be running; a
  watch that never fires is the failure to avoid. Which window of that app
  is in front is the decision under Waiting.
- **Wayland.** Refused even with XWayland running. The compositor tells no
  client which window has focus, and XWayland's `_NET_ACTIVE_WINDOW` covers
  X11 clients only, so it would name a stale app, or none, whenever a native
  window is in front. Some compositors export focus through a protocol of
  their own, but moq-video captures a Wayland window only through the
  portal's picker, which cannot be pointed at one, so the window named could
  not be followed.

Tests:

- **Linux under Xvfb**, in the lane the X11 capture workflow runs in: the
  test stands in for the window manager, sets `_NET_SUPPORTED` and
  `_NET_ACTIVE_WINDOW` on the root, moves it between two windows and to
  none, and the watch reports each app and window in order with no timer. A
  root that does not list `_NET_ACTIVE_WINDOW` is refused at setup.
- **Merge gate, every host:** a Wayland session is refused, with and without
  XWayland.
- **By hand on a Mac and a Windows desktop**, as ignored tests with each
  result in the PR: switching between two apps, between two windows of one
  app, to the desktop, and to a full-screen game.

Public API: additive. Wire: none.

## Waiting

- Maintainer: which window macOS reports as in front. The window server
  orders windows front to back without a permission, but only the
  Accessibility API posts when focus moves between two windows of one app.
  An agent cannot choose it because one option adds a permission prompt to
  every embedder. Options: (1, recommended) report the app by event and its
  front window as of that event, and narrow the Goal, the change contract,
  and the Mac test to say a window switch inside one app is not noticed on
  macOS. A caller can capture the whole app with `Source::App`, but
  `open_app` captures the main display only, so a game on another display is
  missed until the app capture follows the app's display; (2) ask for the
  Accessibility permission and follow the focused window, keeping the Goal
  as written; (3) report the app only on macOS, narrowing the Goal the same
  way as (1). Done when
  this item names the pick.

## Required

- [A capture listing a picker can draw](/quest/m3/capture-list.md) - the app and window identities this reports, on every desktop

## Related

- [Leaving apps out of a display capture](/quest/m4/capture-exclude.md) - names apps by the same identity
