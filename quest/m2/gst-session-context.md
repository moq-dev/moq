# [S] moq-gst playback shares one session context

## Goal

moq-gst's playback helpers take one session context instead of passing six
arguments of session plumbing to each other.

## Plan

Suggested by [moq-dev/moq#7084](https://github.com/moq-dev/moq/pull/7084).
Do it after moqsrc owns each rendition pad in one actor, which reshapes the
same code.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m2 quest, after moqsrc-pad-actor / fold into it / drop.

Public API: none. Wire: none.

## Required

- [moqsrc owns each rendition pad in one actor](/quest/m2/moqsrc-pad-actor.md) - reshapes the playback code this refactors
