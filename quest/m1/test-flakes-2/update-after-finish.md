# [S] A subscribe update racing a finished track ends it cleanly

## Goal

When a publisher finishes a track while the subscriber is writing
SUBSCRIBE_UPDATE on that track's subscribe stream, the subscriber's track ends
cleanly with the publisher's finish, in JS and Rust, instead of failing with the
write's stream error. A real reset of the subscribe stream still fails the
track with the reset's error.

## Plan

Seen in the real-browser rename media case on #5635. `@moq/publish` finished
the old `video` track after a rename. The relay closed the subscribe stream
while the player wrote a SUBSCRIBE_UPDATE on it. The write threw "The stream
was aborted by the remote server". In `js/net/src/lite/subscriber.ts`, the
rejected `#runSubscriptionUpdates` won the `terminal` race, so the track closed
with that error. Watch logged a spawn error, and the media check failed on the
console error. It reproduced in one of five local runs on a loaded machine.

Decided 2026-10-16 (recommended options; the paper trail is in the plan PR):

- A failed update write defers to the subscribe stream's own outcome. If the
  responses end cleanly, the track ends cleanly; if they reject, that error
  wins. No retry and no timeout.
- Fix JS and Rust together. `rs/moq-net`'s `lite/subscriber.rs` serve loop
  (`ServeMode::Select`) hands the track back with the flush error. Confirm
  whether the same race fails a finished track there, and give it the same
  rule.
- Regression tests in both languages: a publisher that finishes the track and
  stops reading while an update is in flight. The subscriber's track ends
  cleanly, and a reset still surfaces its error. Run
  `just test interop --all`, since this is session behavior.

Public API: none. Wire: none.

## Related

- [Browser follows a renamed track](/quest/m1/browser-rename-check.md) - the media check this race failed
