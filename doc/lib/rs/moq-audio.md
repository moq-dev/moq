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
| `encode` | PCM to Opus (mono or stereo, with DTX and voice activity) or raw PCM |
| `decode` | Opus (up to 7.1), PCM, and AAC-LC (mono or stereo) back to PCM, resampled to the rate you want |
| `playback` | One output device mixing every track in a call, with click-free volume ramps |
| `aec` | Acoustic echo cancellation (a port of WebRTC's), so a laptop with no headset doesn't feed itself back |

The microphone opens only while someone listens, and can be swapped without
changing the track subscribers know. Opus packetizes 10 ms frames at the
default low-latency preset. There is no AAC encoder, so an AAC encode request is
refused; AAC decode is for broadcasts from the ingest gateways.

Playback writes never block: samples that do not fit are dropped and
reported, and retrying them would only add latency. Voice activity is read off the Opus
stream, so a call UI needs no second detector. Echo cancellation pairs one
playback engine with one live microphone.

```bash
cargo add moq-audio --features playback
cargo add moq-audio --features capture,playback,aec    # Linux: cpal links libasound
cargo add moq-audio --features capture,pipewire
```

`pipewire` and `pulseaudio` only take effect alongside `capture` or `playback`.

API: [docs.rs/moq-audio](https://docs.rs/moq-audio). Pair with
[`moq-video`](/lib/rs/moq-video). The playout target `moq play` and
`<moq-watch>` share is [audio jitter](/concept/audio-jitter).
