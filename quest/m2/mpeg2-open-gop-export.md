# [S] Open-GOP MPEG-2 export trims broken-link pictures

## Goal

When a TS export starts or resumes at an MPEG-2 sequence header in an open
GOP, it drops or flags the leading B-pictures that reference the previous
GOP, as it already does for codecs whose frames carry presentation time.

## Plan

Found while landing [moq-dev/moq#6252](https://github.com/moq-dev/moq/pull/6252):
whole-PES MPEG-2 frames carry their placement time, not their presentation
time, so the leading-picture check cannot apply. Parse the picture coding type
and temporal reference, or the GOP header's `closed_gop` and `broken_link`
flags, to find them. Low priority: decoders usually skip broken-link
B-pictures.

Public API: none. Wire: none.
