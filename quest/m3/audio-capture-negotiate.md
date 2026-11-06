# [M] Audio capture opens a format the mix accepts

## Goal

moq-audio capture opens any input device at a channel count and sample rate
the bus accepts: a multichannel interface with no stereo or mono format, and
a device offering stereo or mono only at a non-default rate, both capture,
and a caller can pick which discrete inputs to use.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

Without an override, capture opens the device's default config as is
(`rs/moq-audio/src/capture.rs`); with one, `negotiate()` needs an exact match
and otherwise fails `Unsupported`. A device with more than two channels opens
as `Layout::Discrete(n)`, which `Remix` refuses to turn into stereo
(`rs/moq-audio/src/resample.rs`), and `capture::Config` has no input mapping.

Consumer: studio microphones ([moq-dev/studio#27](https://github.com/moq-dev/studio/pull/27)); a 4-input interface
worked before a moq bump via surround guessing and fails now.
