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

`Layout` names speaker meaning separately from a channel count. `Mono` is center,
`Stereo` is left then right, and the surround layouts up to `SevenPointOne`
interleave in the SMPTE/WAVE order (front left, front right, center, LFE, back,
side). A catalog carries only a count, which reads as that count's WAVE default
(`Layout::from_channels`: 6 is 5.1, 8 is 7.1). `Discrete(n)` preserves unnamed
channels without inventing speaker positions, so it passes through but never
remixes. Decoding and playback downmix with the ITU-R BS.775 coefficients and
upmix by leaving the extra speakers silent; the playback mix runs in whatever
layout the output device opened. Encoding keeps source PCM in `encode::Input` and
codec requirements in `encode::Settings`; `encode::Options` adds publication
policy. Decoding likewise separates low-level `decode::Config`, PCM
`decode::Output`, and subscription `decode::Options`.

| Module | Does |
| --- | --- |
| `capture` | Microphones via CoreAudio, WASAPI, ALSA (and PipeWire/PulseAudio hosts), plus macOS system audio |
| `encode` | PCM to Opus (with DTX and voice-activity signaling), raw PCM for the lowest latency, or AAC-LC through a platform encoder |
| `decode` | Opus, PCM, and AAC-LC back to PCM, resampled to the rate you want |
| `playback` | One output device mixing every track in a call, with click-free volume ramps |
| `aec` | Acoustic echo cancellation (a port of WebRTC's), so a laptop with no headset doesn't feed itself back |

`decode` picks a backend per track the way `moq-video` does: a platform decoder
first, then software. `decode::Config::kind` forces one (`Kind::Software`, or
`Kind::Named` with a name below), and `Decoder::name()` reports what opened.

| Backend | Decodes | Hosts |
| --- | --- | --- |
| `libopus` | Opus, mono or stereo, and surround up to 7.1 (channel mapping family 1) | all |
| `pcm` | PCM | all |
| `symphonia` | AAC-LC, mono or stereo (the default-on `aac` feature) | all |

No platform decoder is wired in yet, so multichannel AAC and HE-AAC declared in
its config are refused at construction on every host. HE-AAC signaled only in
band plays as its half-rate LC core. Linux has no OS audio decoder, so it will
stay that way there. Surround Opus is pure Rust, so it is the one multichannel
path every host has. Opus mapping families other than 0 and 1 (ambisonics, and
255's unpositioned channels) are refused, since they declare no speakers.

`encode` selects the same way, through `encode::Settings::kind`, and
`Encoder::name()` reports what opened.

| Backend | Encodes | Hosts |
| --- | --- | --- |
| `libopus` | Opus, mono or stereo | all |
| `pcm` | PCM | all |

`encode::Codec::Aac` is AAC-LC (`mp4a.40.2`) at the input's rate and layout:
mono, stereo, 3.0, 4.0, 5.0, 5.1, or 7.1, the layouts with an AAC
channelConfiguration. Frames are 1024 samples, so `Settings::from_input` sets
`frame_duration` to match. The catalog's AudioSpecificConfig is built from the
settings when the track is registered, and since it has no field for the
encoder's delay, packets are stamped that much earlier so the first input
sample still lands at the first timestamp. There is no software AAC encoder,
and no platform encoder is wired in yet, so `Codec::Aac` is refused at
construction on every host for now. Linux has no OS encoder, so it will stay
that way there.

Highlights:

- **`encode::Publication`** advertises the track and opens the microphone only while someone listens. Stop, swap devices, and restart without changing the track subscribers know; read a level meter for the UI.
- **A/V sync signal.** `Sink::buffered()` reports how far ahead the speaker is, which is what a video clock steers by.
- **Activity per packet**, read off the Opus stream, so a call UI shows who is talking without a second voice detector.
- **One Linux build dependency**: ALSA headers, and only when `capture` or `playback` is enabled.

```rust
let mut audio = moq_audio::decode::Consumer::new(&broadcast, &rendition, "audio", Default::default()).await?;
let engine = moq_audio::playback::Engine::open(Default::default()).await?;
let mut input = moq_audio::playback::Input::default();
input.sample_rate = audio.sample_rate();
input.layout = audio.layout();
let mut sink = engine.sink(input)?;
while let Some(frame) = audio.read().await? {
    let write = sink.write(&frame.data)?;
    if write.dropped_sample_frames > 0 {
        eprintln!("dropped {} live audio frames", write.dropped_sample_frames);
    }
}
```

Playback writes never block. Inspect the returned input sample-frame counts for
telemetry, but do not retry dropped live audio because that would add latency.

For a speakerphone, build one echo-cancellation control set from the playback
engine and give a clone to the microphone configuration. Other clones are safe
for UI toggles, but only one live microphone can attach the adaptive state:

```rust
let aec = engine.canceller(moq_audio::aec::Config::default())?;
let controls = aec.clone();

let mut microphone = moq_audio::capture::Config::default();
microphone.aec = Some(aec);

controls.set_enabled(false); // passthrough without reopening the device
```

A second `Engine::canceller` call, or a second microphone using the same
controls while the first is live, returns `Error::Busy`. Dropping the live
capture frees the microphone slot. The engine slot frees only once every
`Control` clone and the live capture have dropped, since a capture holds the
controls alive.

```bash
cargo add moq-audio --features playback                # decode and play the example above
cargo add moq-audio --features capture,playback,aec    # microphone, speaker, echo cancellation (Linux: cpal links libasound)
cargo add moq-audio --features capture,pipewire         # capture through the PipeWire cpal host
```

`pipewire` and `pulseaudio` only configure cpal when `capture` or `playback`
also enables device I/O. A host flag by itself does not compile or link cpal.

API: [docs.rs/moq-audio](https://docs.rs/moq-audio). Pair with
[`moq-video`](/lib/rs/moq-video).
