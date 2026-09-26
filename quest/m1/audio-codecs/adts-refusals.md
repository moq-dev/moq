# [S] ADTS export refuses what it cannot label

## Goal

`moq-mux`'s ADTS writer never labels an AAC track with the wrong layout or
object type. A channel count ADTS cannot name is refused, not silently written
as stereo, and an explicit SBR or PS description is written with the object
type ADTS can carry or refused, not masked to two bits into a wrong profile.

## Plan

- `channel_config_from_count` falls back to stereo for counts it cannot
  represent; refuse instead, the same way #4178 made channelConfiguration 11 to
  14 refuse.
- The ADTS header masks the object type to two bits, so an explicit HE-AAC
  (object type 5) or HE-AACv2 (29) description is mislabeled. Decide per case:
  signal the backward-compatible AAC-LC core (implicit SBR) when the description
  allows it, otherwise refuse.
- Tests for both with real fixtures, checked against ffprobe.

Public API: none. Wire: none; TS output changes only for inputs it mislabeled.

## Related

- [#4178](https://github.com/moq-dev/moq/pull/4178) - the PCE export that found these
