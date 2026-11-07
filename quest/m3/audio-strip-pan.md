# [S] Pan a bus source

## Goal

A `moq_audio::bus` `Strip` has a pan, set and read like its gain, that
places its source between left and right in every mix that plays it.
Setting a pan other than center on a bus that is not stereo is an error.

## Plan

Decided 2026-11-07 (maintainer, on moq-dev/studio's /quest-complete of
[moq-dev/studio#77](https://github.com/moq-dev/studio/pull/77)):

- ✅ **Pan lives on the strip**, so it applies wherever the sound plays,
  as gain, mute, and delay do. Rejected: a pan per mix the source is
  routed to.
- ✅ **Constant power, -3 dB at center** (a sine/cosine law), so a sound
  keeps its loudness as it moves. Rejected: linear (-6 dB) and 0 dB center.
- ✅ **A stereo source is balanced**: the far side is attenuated by the same
  law and each channel stays where it is. Rejected: moving both channels
  together.
- ✅ **Stereo buses only.** A non-center pan on a mono or surround bus is
  refused with an error. Rejected: ignoring it, and a surround pan nobody
  needs yet.
- ✅ **m3**: additive, with a named consumer.

The consumer is the studio's Audio panel and its `[audio.<source>]` scene
file settings, through
[the studio's pan quest](https://github.com/moq-dev/studio/blob/main/quest/m0/audio-pan.md);
the studio bumps its pin in
[its own quest](https://github.com/moq-dev/studio/blob/main/quest/m0/pin-audio-pan.md).

A `Strip` holds no layout today and meets one only in `Bus::channel`, and
`Driver::advance` has no error path, so the refusal needs a call that
returns it. Recommendation, not settled: `Bus::channel` refuses a strip
already panned off center on a bus that is not stereo, and the strip keeps
a count of its open channels on such buses (counted up in `Bus::channel`,
down when the channel drops), so a later `Strip::set_pan` off center returns
the error while any is open. A strip outlives its channels, so a flag would
keep refusing after the last one closes. The driver never sees a refused pan.

Also recommended, not settled: store the pan atomically beside the gain and
apply both in the same per-frame loop, so a pan change is click-free in the
same way a gain change is. Test a mono impulse panned hard left, center,
and hard right, and a stereo source balanced, against the expected channel
levels.
