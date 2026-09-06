---
title: moq-audio
description: Native audio capture, codecs, playback, and echo cancellation
---

# moq-audio

[![crates.io](https://img.shields.io/crates/v/moq-audio)](https://crates.io/crates/moq-audio)
[![docs.rs](https://docs.rs/moq-audio/badge.svg)](https://docs.rs/moq-audio)

The audio half of a native call: microphone in, hang track out, speaker at
the far end. Everything is Rust, so there is no C toolchain, CMake step, or
codec to install.

| Module | Does |
| --- | --- |
| `capture` | Microphones via CoreAudio, WASAPI, ALSA (and PipeWire/PulseAudio hosts), plus macOS system audio |
| `encode` | PCM to Opus (with DTX and voice-activity signaling) or raw PCM for the lowest latency |
| `decode` | Opus, PCM, and AAC-LC back to PCM, resampled to the rate you want |
| `playback` | One output device mixing every track in a call, with click-free volume ramps |
| `aec` | Acoustic echo cancellation (a port of WebRTC's), so a laptop with no headset doesn't feed itself back |

Highlights:

- **`encode::Publication`** advertises the track and opens the microphone only while someone listens. Stop, swap devices, and restart without changing the track subscribers know; read a level meter for the UI.
- **A/V sync signal.** `Sink::buffered()` reports how far ahead the speaker is, which is what a video clock steers by.
- **Activity per packet**, read off the Opus stream, so a call UI shows who is talking without a second voice detector.
- **One Linux build dependency**: ALSA headers, and only when `capture` or `playback` is enabled.

```rust
let mut audio = moq_audio::decode::Consumer::new(&broadcast, &rendition, "audio", Default::default()).await?;
let engine = moq_audio::playback::Engine::open(Default::default()).await?;
let mut sink = engine.sink(moq_audio::playback::Input { sample_rate: audio.sample_rate(), channels: audio.channels(), ..Default::default() })?;
while let Some(frame) = audio.read().await? {
    sink.write(&frame.data)?;
}
```

```bash
cargo add moq-audio                                    # aac, capture, playback, aec on by default
cargo add moq-audio --no-default-features --features aac   # a service that only relays or decodes
```

API: [docs.rs/moq-audio](https://docs.rs/moq-audio). Pair with
[`moq-video`](/lib/rs/moq-video).
