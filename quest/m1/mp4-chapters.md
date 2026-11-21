# [S] A finished MP4 carries chapters

## Goal

moq-mux's regular MP4 export (`container::mp4`, both `Export::finish` and
crash `repair`) takes a list of chapters, each a moment on the recorded
broadcast's timeline and a title, and writes them into the finished `moov`.
ffmpeg, mpv, VLC, and Apple's players (QuickTime Player, Safari, Photos) list
them. Reading chapters back on import is out of scope.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan): ✅ write both forms, Nero's
`chpl` box in the `moov`'s `udta` and a QuickTime chapter text track linked
from the video track by `tref chap` (rejected: `chpl` only, which Apple's
players ignore; the QuickTime track only, which some editors ignore).

moq-mux alone knows where the movie starts on the broadcast's timeline, so it
maps each chapter's moment onto the movie's time; a chapter before the first
sample or after the last is dropped, not clamped. mp4-atom already encodes
`chpl`. Prove it with ffprobe listing the chapters of a finished and of a
repaired file.

Found by [moq-dev/studio#209](https://github.com/moq-dev/studio/pull/209),
whose markers become a recording's chapters through
[the studio's pin](https://github.com/moq-dev/studio/blob/main/quest/m2/pin-mp4-chapters.md).
