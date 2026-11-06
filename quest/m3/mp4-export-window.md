# [M] MP4 export trims exactly at both ends

## Goal

An MP4 export takes a presentation window and presents exactly that span: no
keyframe lead-in before its start, and frames past its end kept only as
references for B-frame decoding, never presented.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`mp4::Export` (`rs/moq-mux/src/container/mp4.rs`) already writes an edit list
from its `Timeline`, which starts at the earliest presentation time and covers
every written sample. There is no caller-supplied window: an export starts
where its subscription starts, keyframe included, and `stop()` is the only
end.

Consumer: studio replay clips ([moq-dev/studio#20](https://github.com/moq-dev/studio/pull/20)) cut up to about
200 ms early today to avoid presenting the lead-in. Test with B-frame content
that the trimmed file plays the window and nothing else in at least one
standard player.
