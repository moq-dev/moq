# [M] A decode backend seam that prefers the platform codec

## Goal

`moq_audio::decode` selects a backend per codec the way `moq_video::decode`
does: platform first, software fallback, and a `Kind` to force one. Opus,
PCM, and symphonia AAC-LC become the software backends, and the crate
documents which codecs each host decodes.

## Plan

Mirror `rs/moq-video/src/decode/backend` in name and shape: a crate-private
`Backend` trait (`decode`, `flush`, `name`), an `open(codec, config)` that
walks the platform candidates before the software ones and refuses when none
takes the track, and `decode::Kind { Auto, Platform, Software, Named }` on
`decode::Config`. `Decoder::name()` reports what was opened, which the OBS
stats and `moq play` surface.

- The seam is generic over `hang::catalog::AudioCodec`, so a backend advertises
  the set it opens and the selector asks each in order. Symphonia advertises
  AAC-LC mono/stereo only; the platform backends that follow advertise what
  their framework opens and has a fixture for.
- Move today's Opus, PCM, and symphonia code behind the trait without changing
  behavior; the HE-AAC sniff from [HE-AAC refusal](/quest/m2/audio-codecs/he-aac-refusal.md)
  lands in the symphonia backend.
- A backend's output rate and layout are what it produced, not what the
  catalog said (HE-AAC doubles the rate); `Consumer` already resamples and
  remixes to the requested output, so that stays the seam's contract.
- The `aac` feature keeps gating symphonia. Platform backends are
  `cfg(target_os)` like their video counterparts, with `mediacodec` behind the
  existing feature.
- Docs: `doc/lib/rs/moq-audio.md` gains the backend table `moq-video.md` has,
  and states the Linux gap. `doc/bin/cli.md` and `doc/bin/obs.md` follow.
- Regression: the selection order and `Named` refusal, tested with a stub
  backend like the video seam's `probe`.

The FFI does not expose `Kind` until a consumer asks.

## Related

- [Layout](/quest/m2/audio-codecs/layout.md) - independent; the platform backends need both
