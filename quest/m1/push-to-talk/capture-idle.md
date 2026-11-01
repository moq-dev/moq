# [S] Hidden capture idles

## Goal

While nothing reads it (the camera is invisible or every encoder is disabled),
`Video.Capture` produces no frames, yet unhiding resumes as quickly as it does
today.

## Plan

[moq-dev/moq#7157](https://github.com/moq-dev/moq/pull/7157) made `muted` and
`invisible` keep capture open so unmute and unhide skip getUserMedia. The
capture now keeps producing frames nobody reads. Keep the device open but stop
pulling and processing frames while unread, and measure the idle CPU before and
after.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m1 push-to-talk child / drop.

Public API: none. Wire: none.
