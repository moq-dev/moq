# [S] AAC config encode refuses unnameable channel counts

## Goal

`moq_mux::codec::aac::Config::encode` never writes an AudioSpecificConfig that
names the wrong layout. A channel count no channelConfiguration names is
refused, not logged and written as stereo, the way the ADTS writer refuses it.

## Plan

- `encode` returns `Bytes` today, so refusing is a published API break.
  Decide whether it returns a `Result` or whether a
  constructor validates the count up front so encoding cannot fail.
- A count a program config element could describe (7, or more than 8) is
  still refused unless the caller supplies a layout; guessing speaker
  positions is what this removes.
- Callers in `moq-audio` (encoder config, description synthesis) already
  validate first; check they keep their error messages. `Producer` synthesizes
  the ASC at construction, so it refuses such a layout there.
- JS already matches: `@moq/hang`'s `audioSpecificConfig` throws for the same
  counts (#4119). Test every count from 1 to 8 and one beyond.

Public API: `Config::encode` (or its constructor) changes. Wire: none.

## Related

- [#4283](https://github.com/moq-dev/moq/pull/4283) - the ADTS export refusals this mirrors
