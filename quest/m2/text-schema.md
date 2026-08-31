# [M] Text schema

## Goal

Hang exposes a native text rendition and timing contract for independently
hosted transcription. A transcription contribution can carry text without
claiming media groups it does not serve.

## Plan

Define and land the text rendition, timing, relative-broadcast, and
contribution schema. Text shares the source PTS and wall-clock epoch but keeps
its own cue/group identity and publishes a local availability index rather
than copying an audio or video timeline. Composing transcription contributions
onto broadcasts stays downstream in moq.pro.
