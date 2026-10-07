# [S] TS Opus export refuses what it cannot label

## Goal

`moq-mux`'s MPEG-TS exporter never labels an Opus track with a channel
configuration its packets do not have. A track whose OpusHead the Opus
extension descriptor can describe exports as today; anything else is refused,
not written with a guessed `channel_config_code`.

## Plan

- The exporter derives `channel_config_code` from the catalog channel count
  alone, clamped to 1..=8, so a family 255 or ambisonics head, a family 1 head
  with a non-Vorbis table, or more than eight channels exports mislabeled.
  Decide from the parsed description: family 0, and family 1 with the Vorbis
  default mapping, keep the plain code; refuse the rest. Writing the explicit
  0x81 layout (Opus-in-TS draft Table 4-2), which import now reads, can wait
  until a demuxer in the path reads it; ffmpeg does not.
- A track with no description stays mono or stereo only.
- Tests with real heads, checked against ffprobe where ffmpeg reads the result.

Public API: none. Wire: none; TS output changes only for inputs it mislabeled.
