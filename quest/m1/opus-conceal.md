# [XS] Opus concealment length

## Goal

`moq_audio` `Decoder::decode(&[])` conceals one Opus packet, the length of the
last real one, instead of 120 ms. The API is unchanged and lands on main.

## Plan

- `rs/moq-audio/src/decode/decoder.rs` passes `max_frame_size` (120 ms) as
  libopus's `frame_size` on every call, and libopus conceals exactly that many
  samples. Record the last packet's sample count (`opus_packet_get_nb_samples`)
  and pass it for an empty packet; it is always a multiple of 2.5 ms.
- Refuse loss before any packet has decoded, rather than surfacing libopus's
  `BUFFER_TOO_SMALL`.
- Document on `decode` how much an empty packet conceals.
- Regression from the issue's repro: one lost 20 ms packet yields 20 ms.
- A `conceal(duration)` method and `decode(Option<&[u8]>)` were rejected: no
  consumer needs a caller-chosen length, and the latter breaks the common case.
  How platform backends signal loss belongs to the decode-backend seam.

## Closes

- [#4248](https://github.com/moq-dev/moq/issues/4248) - `Decoder::decode(&[])` conceals 120 ms of Opus audio for one lost packet

## Related

- [Audio loss recovery](/quest/m2/audio-loss-recovery.md) - the FEC versus concealment policy study
- [Decode backend](/quest/m1/audio-codecs/decode-backend.md) - where platform decoders signal loss
