# [S] js/watch: an audio-only stream resumes every time the publisher unmutes

## Goal

A subscriber on an audio-only broadcast keeps playing each time the publisher
toggles `muted` back to `false`, not only the first time. Today the second and
every later unmute is detected, played, and immediately closed, until the
publisher reloads its page, which buys exactly one more resume.

## Plan

Reproduce with `just relay`, a `<moq-watch>` on a `hang`, and a `<moq-publish>`
started `invisible` and `muted`, toggling `muted` a few times. Two candidate
mechanisms, and the fix depends on which one it is:

- The publisher side retracts and re-creates the audio rendition or its
  track on each toggle, and `js/watch` treats the second appearance of the
  same rendition name as the end of the one it was playing (a stale
  `closed` handler from the first subscription evicting the second, the
  shape [#2985](https://github.com/moq-dev/moq/issues/2985)
  describes on the publisher side).
- The watch audio backend keys its "already started" state on the rendition
  and never re-arms after the track ends, so the resubscribe races its own
  teardown.

Find which by logging track lifetimes on both ends, fix it where the state
goes stale rather than by retrying the subscribe, and add a `js/watch` test
that unmutes three times and asserts audio frames keep arriving after each.

## Closes

- [#3363](https://github.com/moq-dev/moq/issues/3363) - close this issue when the quest finishes
