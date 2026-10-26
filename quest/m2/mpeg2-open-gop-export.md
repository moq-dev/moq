# [S] Open-GOP MPEG-2 export trims leading B-pictures

## Goal

When a TS export starts or resumes at an MPEG-2 sequence header in an open
GOP, it drops the leading B-pictures that reference the previous GOP and
counts them as `trimmed` or `dropped`, as it already does for codecs whose
frames carry presentation time.

## Plan

Found while landing [moq-dev/moq#6252](https://github.com/moq-dev/moq/pull/6252):
whole-PES MPEG-2 frames carry their placement time, not their presentation
time, so the leading-picture check cannot apply. A GOP header with
`closed_gop` unset marks an open GOP; its leading pictures are the B-pictures
(picture coding type) whose temporal reference precedes the I-picture's.
`broken_link` is the encoder's own flag for an edited stream, not the trigger.
Cover it with an MPEG-2 open-GOP arm in `recovery_test.rs`, graded like the
H.264 `Open` arm. Low priority: decoders usually skip these B-pictures.

Public API: none. Wire: none.
