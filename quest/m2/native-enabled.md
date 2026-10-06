# [M] Native consumers skip disabled renditions

## Goal

Native consumers (moq-cli play, moq-gst, the moq-ffi and C convenience paths,
and audio selection generally) never select a rendition with
`enabled: false`, as `@moq/watch` does after #4915.

## Plan

#4915 makes `hang::catalog::Video::ranked()` sort disabled renditions last, so
transcode, rtc egress, rtmp, and FLV export prefer an enabled one. These
consumers select without it. Route their selection through one ranked helper
for video and audio rather than adding a check at each site. A C
`moq_consume_audio_enabled` getter rides along if a C consumer needs it.
