# [XS] Watch caches audio codec support

## Goal

`js/watch`'s audio source checks codec support once per rendition config, as
video does, so an unmute resumes the AudioContext without waiting on a support
check.

## Plan

Found 2026-10-28 by [moq-dev/moq#7155](https://github.com/moq-dev/moq/pull/7155):
the audio source re-checks codec support on every catalog change, while video
caches the result. So on unmute the AudioContext resumes only after a support
check of a few milliseconds, even though frames are already arriving. Share
the cache with video if that keeps it simpler.

Public API: none. Wire: none.
