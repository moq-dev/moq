# [XS] An audio-only broadcast that can't play hides its play button

## Goal

`js/watch`'s `center-play` hides for an audio-only broadcast that can't play,
as it does when video has an error. The unsupported notice from
[moq-dev/moq#7359](https://github.com/moq-dev/moq/pull/7359) then stands alone.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Use #7359's rule: a broadcast with no video renditions counts as audio.
