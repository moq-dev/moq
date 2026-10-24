# [S] Interop tone check catches a misapplied pre-skip

## Goal

`test/interop/tone.py` fails a subscriber whose decoder drops or keeps the
wrong number of priming samples (Opus pre-skip, AAC priming), the way it
already fails a channel swap or a gap. Today a wrong pre-skip only shifts a
steady tone's phase, which the energy-and-frequency check cannot see.

## Plan

Found in review of the interop audio checks. The publishers' tone phase is a
function of the publisher's timestamps (the FFI clients and ffmpeg's `aevalsrc`
both start at zero), so the decoded sample at timestamp `t` should carry phase
`2πft`: anchor the Goertzel phase to the frame timestamps.

Things to look out for:

- Precision differs per client: the libav fallback stamps milliseconds (about
  1.4 rad at 440 Hz), and the browser publisher's capture clock is not tied to
  its WAV's phase. Check only where the timestamps are precise enough, and say
  which clients are left out.
- Add a negative control that misapplies the pre-skip on purpose, so the check
  is proven able to fail.
- The GStreamer subscriber likely fails it today: `opusdec` trims pre-skip only
  from a buffer's `GstAudioClippingMeta`, never from the OpusHead, and `moqsrc`
  attaches none.

Public API: none expected. Wire: none.

## Related

- [Interop AAC](/quest/m1/interop-aac.md) - AAC priming is the same question
