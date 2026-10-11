# [XS] The watch buffering spinner never covers the paused play button

## Goal

While `<moq-watch>` is paused, its buffering indicator stays hidden, so the
centre play button is always clickable, and Interop's resume step never times
out on it.

## Plan

Found in #5174 (2026-10-10): in Interop run 38013145379 the `cpp -> js` cell
failed because, after pause, audio stalling raised the buffering spinner
(`js/watch/src/ui/components/buffering-indicator.ts`) over the play button,
and the harness's resume click timed out. Planned in m0, decided 2026-10-10,
because it reddens Interop on unrelated PRs.

Buffering is not a meaningful state while paused: hide the indicator whenever
playback is paused, and show it again only once playback resumes and still
stalls. Add a browser test that pauses during an audio stall and clicks
resume. No harness workaround such as clicking elsewhere or waiting longer.

Public API: none. Wire: none.
