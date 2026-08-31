# [L] moq-audio: echo cancellation / AGC / noise suppression for native capture

## Goal

Implement and verify the behavior tracked in [#2282](https://github.com/moq-dev/moq/issues/2282)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

You cannot build a native MoQ conferencing client today, because two participants on speakers will howl. The browser gets echo cancellation, noise suppression, and auto gain control free from getUserMedia; native gets nothing. That asymmetry blocks every native two-way use case.

#### What exists today

**`rs/moq-audio` does codec + format + resample, and no processing:**

- `codec.rs`  -  Opus `Encoder`/`Decoder` via `unsafe-libopus` 0.2 (pure-Rust transpile of libopus 1.3.1)
- `format.rs`  -  `AudioFormat` (mirrors WebCodecs `AudioData.format`), layout conversion to interleaved f32
- `resample.rs`  -  `Resampler`, rubato 3.0, sinc only. **Sample-rate only**; channel up/downmix is rejected upstream.
- `producer.rs` / `consumer.rs`, `frame.rs`, `error.rs`
- `capture.rs` + `capture/permission.rs`

No echo cancellation, no AGC, no noise suppression, no VAD, no mixer. The only signal-conditioning-adjacent knobs are Opus-internal (DTX, `signal=voice`), and those are **JS-side only** (`js/publish/src/audio/types.ts`, `Kind = "voice"|"music"|"auto"`)  -  the native encoder has no equivalent plumbing.

**Native mic capture already exists**, which is what makes this gap actionable rather than theoretical: `rs/moq-audio/src/capture.rs` (behind the `capture` feature) uses **cpal 0.18** (CoreAudio/WASAPI/ALSA) with `capture::Config { device, sample_rate, channels }` and `capture::Microphone` (realtime callback → `mpsc` → interleaved-f32 `Frame`s). `capture/permission.rs` handles macOS TCC up front (objc2 + objc2-av-foundation), and `FIRST_BUFFER_TIMEOUT` = 5s surfaces a denial as an error instead of a silent hang. Wired to the CLI via `ImportSource::Capture` (`--microphone`, `--audio-bitrate`).

So we can capture a mic natively. We just can't make it usable in a two-way call.

**Browser**: `js/publish/src/source/microphone.ts:44` calls `getUserMedia({ audio: finalConstraints })` where constraints come from `MicrophoneProps.constraints` (`Audio.Constraints`, default `{}`). So `echoCancellation`/`noiseSuppression`/`autoGainControl` are *passable by the app* but **moq sets no defaults**  -  you get the browser's implicit behavior (Chrome defaults them on for `{audio: {...}}`, but that's the browser's choice, not ours). `TrackSettings` (`js/publish/src/audio/types.ts:44`) reads them back, all optional since Safari underreports.

**No audio-processing crate is in the tree.** Grepping every `rs/*/Cargo.toml` for `webrtc-audio-processing`, `speexdsp`, `rnnoise`, `nnnoiseless`, `earshot`, `aec`, `agc` returns zero hits. `rs/moq-rtc` (str0m) has none either.

#### The hard constraint

`rs/moq-audio`'s stated stance in its `Cargo.toml`: pure-Rust, no CMake, no C linker hackery, "audio never touches ffmpeg". That's a good rule and it's load-bearing for cross-compilation (Android/iOS/Windows).

It also rules out the obvious answer. `webrtc-audio-processing` is the industry-standard AEC and it's a C++ binding needing CMake and abseil. Adopting it would break the invariant hard.

Rough state of the pure-Rust options:

- **NS**: `nnnoiseless` (RNNoise port)  -  viable, pure Rust.
- **VAD**: `earshot` (WebRTC VAD port)  -  viable, pure Rust.
- **AGC**: simple enough to implement directly; it's a feedback loop over gain, not signal processing.
- **AEC**: **no good pure-Rust option exists.** This is the real problem. AEC is also the one that actually matters for conferencing  -  NS and AGC are nice, AEC is the difference between "works" and "unusable".

#### Open questions (the decision is question 1)

1. **How much is the pure-Rust stance worth?** Three ways out:
   - Take the C++ dep (`webrtc-audio-processing`) behind an **optional, off-by-default feature**, so the default build keeps the invariant and anyone who wants AEC opts in and eats CMake. This is probably the pragmatic answer and it's what the feature-gating in `moq-video` already does for NVENC/VAAPI.
   - Write a pure-Rust AEC. Real work (adaptive filter, double-talk detection, nonlinear processing) and easy to do badly.
   - Punt AEC and ship NS + AGC + VAD pure-Rust. Half a loaf, and it does not unblock conferencing.
2. **Does this belong in `moq-audio` at all?** It's an audio *pipeline* concern, not a codec concern. Maybe a separate `moq-audio-dsp` crate, or a `processing` module gated behind a feature. Keeping `moq-audio` a clean pure-Rust codec crate has value.
3. **AEC needs the far-end reference signal**, which means the *playback* path has to hand its samples to the *capture* path. `moq-audio` has no playback/output side at all today (no speaker output, no mixer)  -  so native conferencing needs that first, and this issue may be blocked on it. Worth checking before scoping.
4. **Should `js/publish` set explicit defaults** rather than inheriting browser behavior? Probably yes for `echoCancellation`  -  silently depending on a browser default that Safari reports differently is a bug waiting to happen. Cheap and separable from all of the above.
5. Platform AEC is an alternative worth pricing: macOS/iOS `kAUVoiceIOProperty_*` (Voice Processing I/O), Windows `AEC` DMO, Android `AcousticEchoCanceler`. Better quality than anything we'd write, no C++ dep, but three platform backends and cpal doesn't expose them  -  it'd mean bypassing cpal on those paths, mirroring how `moq-video/src/capture/` uses platform APIs directly.

Option 5 is arguably the most in keeping with how `moq-video` already does capture/encode (platform-native backends, software fallback), and it dodges the C++ dependency entirely. It's also the most work.

#### Branch

`main`  -  additive (new feature flag / new module / new crate).

#### Cross-package sync

`rs/moq-audio` → `doc/`; if it reaches the FFI surface, the `moq-ffi` row (`rs/libmoq`, `{py,swift,kt}/`, `go/wrapper/moq/*.go`, `doc/lib/{py,swift,kt,go,c}`). The `js/publish` default-constraints piece is independent.

## Closes

- [#2282](https://github.com/moq-dev/moq/issues/2282) - close this issue when the quest finishes
