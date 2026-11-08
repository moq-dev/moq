# [XS] Video's declared-end freeze is not a stall

## Goal

`<moq-watch>` doesn't count a stall while video holds its last picture
through a publisher's declared end, as audio already doesn't for a declared
silence. The stall count, stalled time, and the echo `stalls`/`stalled`
feedback agree between audio and video.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8209](https://github.com/moq-dev/moq/pull/8209), which made
watch count a stall only while a rendition plays. It left this one case as
on `main`: a picture held through a declared end for longer than the 500 ms
buffering timer still counts as a stall. Give video's playback state a
declared-end phase, as audio's handles a declared silence, so the buffering
timer doesn't run there. Test a declared end held past 500 ms.

Public API: none. Wire: none.
