# [S] Audio device tests pass on ARM

## Goal

`just rs audio-devices` passes on the hosted ARM runner, as it does on
x86_64 Linux, so its nightly lane is a signal rather than a known red.

## Plan

The recipe first ran nightly on 2026-10-20 and failed all five playback
tests on ARM; nothing owned it
([moq-dev/moq#6155](https://github.com/moq-dev/moq/pull/6155)).
[moq-dev/moq#6175](https://github.com/moq-dev/moq/pull/6175) then fixed
PulseAudio's 20 ms playback period for these tests.

But Nightly
[39600361153](https://github.com/moq-dev/moq/actions/runs/39600361153)
(2026-10-21, before #6175 merged) already passed all five on
`ubuntu-24.04-arm`, after the 2026-10-20 failure, with no moq-audio change
between. So a green nightly alone does not prove #6175 fixed it: the failure
may be intermittent. Decided 2026-10-22 (maintainer): delete this quest only
after three consecutive green Nightlies on ARM; on another failure, reproduce
on ARM and fix the cause, not the timing.

Public API: none. Wire: none.

## Related

- [PulseAudio capture periods](/quest/m2/pulseaudio-capture-period.md) - the capture half of the same period fix
