# [S] Wayland display id

## Goal

Asking moq-video to capture a specific display off X11 either captures that
display or fails loud. It never silently opens the screen portal instead,
and one publication's portal choice never leaks into another's.

## Plan

Found 2026-10-22 by the API audit (a regression since release). In
`rs/moq-video/src/capture/mod.rs`, `Source::Display(Some(id))` without X11
only logs at debug level and opens the portal, and `pipewire.rs` keeps a
process-global `static DISPLAY` portal selection shared across publications.

- Refuse `Display(Some)` off X11 with a typed error, and have moq-cli pass
  `Source::Portal(Selection::new(Kind::Screen))` there, as `--window` already
  does.
- Scope the portal selection to its publication.

## Related

- [Linux capture](/quest/m2/capture-linux.md) - the broader Linux capture work
