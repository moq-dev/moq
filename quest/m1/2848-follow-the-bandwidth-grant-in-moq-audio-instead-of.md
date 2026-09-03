# [M] Follow the bandwidth grant in moq-audio instead of holding a fixed reservation

## Goal

Implement and verify the behavior tracked in [#2848](https://github.com/moq-dev/moq/issues/2848)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to [#2815](https://github.com/moq-dev/moq/issues/2815), which divides a connection's send-bandwidth estimate among the tracks sharing it.

In that first pass `moq-audio` registers a share at its configured bitrate and then ignores the grant: it reserves, but never follows. That keeps the video encoder's share honest (audio is no longer invisible to it), which is most of the value, but it means a link too small for the configured audio rate has no way to shed audio bits.

#### What to add

Opus can retune live. `moq_audio::encode::encoder` already has `set_opus_bitrate` (valid range 500 bps up to a per-channel-count max) and reads the value back with `OPUS_GET_BITRATE`, so following a grant is a matter of feeding it through a rate policy at the same point `moq-video` does, rather than any new codec work.

Reuse `moq_video::encode::rate::Control` rather than writing a second policy, or lift it somewhere both crates can reach. Its attack/decay shape (drops apply at once, raises ramp) is codec-agnostic, and having two rate policies drift apart is exactly the problem [#2815](https://github.com/moq-dev/moq/issues/2815) was closing.

#### What can't adapt

PCM. `pcm::bitrate(codec_rate, codec_channels)` is fixed by sample rate and channel count, and `Config::bitrate` is rejected outright for it. So a "reserves but never follows" share has to remain expressible regardless; this issue is about Opus opting into following, not about removing the fixed case.

#### Priority

Low. `hang::catalog::PRIORITY` puts audio at 80 and video at 60, so the allocator satisfies audio's reservation before video sees a bit. Audio only gets squeezed once the link can't carry audio alone, at which point the picture is long gone. Worth doing for the tail case, not worth blocking on.

## Closes

- [#2848](https://github.com/moq-dev/moq/issues/2848) - close this issue when the quest finishes

