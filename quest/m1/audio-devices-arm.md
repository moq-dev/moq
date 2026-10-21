# [S] Audio device tests pass on ARM

## Goal

`just rs audio-devices` passes on the hosted ARM runner, as it does on
x86_64 Linux, so its nightly lane is a signal rather than a known red.

## Plan

The recipe first ran nightly on 2026-10-20 and failed all five playback
tests on ARM; nothing owned it
([moq-dev/moq#6155](https://github.com/moq-dev/moq/pull/6155)).
[moq-dev/moq#6175](https://github.com/moq-dev/moq/pull/6175) then fixed
PulseAudio's 20 ms playback period for these tests. Check a nightly after
#6175 first: if ARM passes, delete this quest. Otherwise reproduce on ARM
and fix the cause, not the timing.

Public API: none. Wire: none.

## Related

- [PulseAudio capture periods](/quest/m2/pulseaudio-capture-period.md) - the capture half of the same period fix
