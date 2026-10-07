---
title: moq-audio
description: Native audio capture, codecs, playback, and echo cancellation
---

# moq-audio

[![crates.io](https://img.shields.io/crates/v/moq-audio)](https://crates.io/crates/moq-audio)
[![docs.rs](https://docs.rs/moq-audio/badge.svg)](https://docs.rs/moq-audio)

The audio half of a native call: microphone in, hang track out, speaker at
the far end. `moq import capture` and `moq play` use it. There is no C
toolchain or codec package to install, apart from ALSA headers on Linux when
capture or playback is enabled.

| Module | Does |
| --- | --- |
| `capture` | Microphones via CoreAudio, WASAPI, ALSA (and PipeWire/PulseAudio hosts), plus macOS system audio |
| `encode` | PCM to Opus (with DTX and voice-activity signaling), raw PCM, or AAC-LC through a platform encoder |
| `decode` | Opus, PCM, and AAC-LC back to PCM, resampled to the rate you want |
| `playback` | One output device mixing every track in a call, with click-free volume ramps |
| `aec` | Acoustic echo cancellation (a port of WebRTC's), so a laptop with no headset doesn't feed itself back |

The microphone opens only while someone listens, and can be swapped without
changing the track subscribers know. Opus packetization is 10 ms at the
default low-latency preset and 20 ms otherwise. That is packetization, not a
delay guarantee: Opus adds its lookahead either way. AAC frames stay 1024
samples.

What actually decodes and encodes today:

- **Opus** decode is mono, stereo, or surround up to 7.1 on every host. Encode is mono or stereo. Ambisonics and unpositioned channel mappings are refused.
- **PCM** in both directions.
- **AAC-LC** decodes mono or stereo. HE-AAC signaled only in band plays as its half-rate LC core. Encoding AAC is refused on every host until a platform encoder is wired in, and Linux has no OS encoder to wire.

Playback writes never block. Dropped live samples are reported and should not
be retried, since a retry would add latency. `Sink::buffered()` is how far
ahead the speaker is, which is what a video clock steers by. Voice activity is
read off the Opus stream, so a call UI does not need a second detector.

Echo cancellation is one control set per playback engine and one live
microphone. A second microphone, or a second canceller on the same engine,
is refused until the first capture is dropped. Disabling it is a passthrough;
the device stays open.

```bash
cargo add moq-audio --features playback
cargo add moq-audio --features capture,playback,aec    # Linux: cpal links libasound
cargo add moq-audio --features capture,pipewire
```

`pipewire` and `pulseaudio` only take effect alongside `capture` or `playback`.

API: [docs.rs/moq-audio](https://docs.rs/moq-audio). Pair with
[`moq-video`](/lib/rs/moq-video). The playout target `moq play` and
`<moq-watch>` share is [audio jitter](/concept/audio-jitter).
