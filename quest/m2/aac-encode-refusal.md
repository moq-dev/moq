# [S] AAC encode refuses a channel count it cannot name

## Goal

Writing an AudioSpecificConfig for a channel count that no AAC
channelConfiguration names is an error, not a stereo config with a warning,
in `moq_mux::codec::aac::Config::encode` and `@moq/hang`'s
`audioSpecificConfig`. This mirrors the parse side, which since #4093 refuses
reserved values instead of guessing stereo.

## Plan

Both functions become fallible, a published API break in each language, so
this targets `dev`. Counts with a PCE-free configuration map as today. For
the others, either write channelConfiguration 0 with a PCE derived from the
layout, or refuse. Pick one at PR time and apply it in both languages. Test
every count from 1 to 8 and one beyond.

## Related

- [AAC PCE](https://github.com/moq-dev/moq/pull/4093) - the parse half
