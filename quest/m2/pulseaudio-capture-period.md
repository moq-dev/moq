# [S] PulseAudio capture periods

## Goal

Capture through cpal's PulseAudio host (moq-audio's opt-in `pulseaudio`
feature) delivers microphone audio in periods of tens of milliseconds, not
the server's default fragment, and a capture test in `just rs audio-devices`
proves it against the private null sink's monitor source.

## Plan

Found by the m1 [coverage audit](/quest/m2/audit-test-coverage.md). Playback
on that host asked for `BufferSize::Default`, which cpal's PulseAudio host
maps to the server's default buffer attributes: a 2 s `tlength`, 3.96 s end
to end. The audit fixed playback (`playback::device::buffer_size` asks for
20 ms periods). Capture (`rs/moq-audio/src/capture.rs`, `resolve`) also opens
with the default buffer size, and PulseAudio's default record `fragsize` is
the same 2 s, so capture likely arrives in 2 s bursts. Echo cancellation
needs both halves within its delay window, so this matters for `aec` too.

- Reproduce first: capture `moq-null.monitor` under `test/lib/pulse.py`
  while a playback engine plays a tone, and measure the callback cadence.
  The pulse log's `Requested fragsize` line shows what the stream asked for.
- If it reproduces, share the playback rule (only the PulseAudio host, a
  20 ms period clamped to the reported range) between capture and playback
  rather than copying it, since `capture` builds without `playback`.
- Add the capture test to the `audio-devices` recipe's selection, waiting
  for the device to start as the playback tests do.

## Related

- [A cpal release loads libasound at runtime](/quest/m2/cpal-alsa-runtime.md) - would make the PulseAudio host the default on hosts without libasound
