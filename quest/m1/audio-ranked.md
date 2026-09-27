# [S] Audio rendition pick

## Goal

An egress that carries one audio rendition (single-track FLV export and RTMP
play, WHEP) serves the best one it supports, not the first by track name.

## Plan

Video already shares `hang::catalog::Video::ranked`. Decide what "best" means
for audio (bitrate, then sample rate and channels are candidates) and add the
matching `Audio` ranking. RTMP's play check still checks the first audio
rendition by name; narrow it to what the client advertised, as video does. Test
with a catalog whose weaker rendition sorts first.
