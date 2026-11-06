# [M] Audio track changes are gapless

## Goal

When a browser publisher changes audio sample rate or channel count, or a player switches audio renditions, playback has no silence beyond one decoder frame, and `just test media` fails when the longest gap exceeds that.

## Plan

Decided 2026-10-30 (maintainer, from [moq-dev/moq#7374](https://github.com/moq-dev/moq/pull/7374)): land the media QA cases with the gap reported, then fix the gap here. Today's longest gaps: 250 ms on a sample-rate change, 250–300 ms on a channel change, 50–70 ms switching to a second rendition, 200–280 ms switching back, and 200 ms native Opus to AAC. On the sample-rate change the publisher briefly drops audio from its catalog.

- Publisher: keep the old audio track listed until the new one has its first group, so the catalog never goes without audio.
- Player: start decoding the new track before stopping the old one, and switch at a frame boundary.
- Turn #7374's `longest audio gap` report into a failing check with a one-frame bound.

Public API: none expected. Wire: none.
