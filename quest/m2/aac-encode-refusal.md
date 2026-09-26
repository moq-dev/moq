# [S] AAC encode refuses a channel count it cannot name

## Goal

Writing an AudioSpecificConfig for a channel count that no AAC
channelConfiguration names is an error, not a stereo config with a warning,
in `moq_mux::codec::aac::Config::encode`, as `@moq/hang`'s
`audioSpecificConfig` already refuses since #4119. This mirrors the parse
side, which since #4093 refuses reserved values instead of guessing stereo.

## Plan

`Config::encode` becomes fallible, a published API break, so this targets
`dev`. Counts with a PCE-free configuration map as today. Refuse the others,
matching JS; writing channelConfiguration 0 with a PCE derived from the layout
is a later additive change in both languages. Test every count from 1 to 8 and
one beyond.

## Related

- [AAC PCE](https://github.com/moq-dev/moq/pull/4093) - the parse half
- [Layout](/quest/m1/audio-codecs/layout.md) - the layout a PCE would be derived from
