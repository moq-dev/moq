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


Implementation checkpoint (2026-11-01): the draft PR parks the capture pump after
its first metadata/replay frame when its last reader leaves, preserves the device
and held picture, and removes prefetch from the native, worker, and element
processor wrappers. Chromium native and forced element-path browser checks cover
idle frame processing, multiple readers, live device retention, replay and fresh
frame resume, and timestamp order. The worker path has deterministic unit coverage;
a real Safari worker has not been measured.

Decision needed before completing this quest: pausing a `FrameSource` exposes the
file source's catch-up behavior. With a manual clock, an image unread for one
minute produces 1,799 overdue frames before its timestamp reaches the present.
`source/file.ts`'s media pacing loop likewise resumes sequential decode from its
old cursor. Current draft code therefore must not land as the completed fix.

Options for the maintainer:

- Recommended: retain demand gating for every source, and make file sources
  skip/seek to the current presentation time when resumed, keeping audio and
  video on their shared timeline. Cover image and decoded-media resume in the
  browser before completing this quest.
- Narrow demand gating to capture devices and keep `FrameSource` inputs draining.
  This preserves current file behavior but narrows the Goal's unread-capture
  promise and leaves synthetic/file inputs doing idle work.

Public API: none. Wire: none.
