# [S] Native audio conceals short losses

## Goal

When groups are lost, `moq_audio::decode::Consumer` fills the gap with Opus
packet loss concealment (PLC) instead of resetting the decoder to silence.
Gaps the publisher declared, such as a pause or DTX silence, still play as
silence. Other codecs keep today's behavior.

Not here: `@moq/watch`, since WebCodecs' `AudioDecoder` has no PLC or FEC
input; Opus in-band FEC (LBRR) and DRED.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan after a group-duration sweep).
On real speech (`rs/moq-audio/src/decode/test_data/opus-speech.f32`), a lost
frame played as silence measured 47 dB log-spectral distance from the
original. PLC brought 20 to 60 ms gaps down to 22 to 28 dB, and libopus fades
its concealment out past about 100 ms. Opus in-band FEC was rejected: within a
QUIC stream, frame N never arrives without N-1, so FEC could only cover the
frame just before a group boundary. It also takes bits from clean speech.

Today the `Encoded::Gap` arm in `rs/moq-audio/src/decode/consumer.rs` calls
`restart()` and emits `Read::Gap`. `decoder.rs` already supports
`decode(&[])` for PLC.

- Tell lost media apart from declared silence. A gap past a declared end is
  the fresh encoder's silence, and so is a pre-lite-07 skip across a pause
  (see the comment on that arm). Those keep the reset.
- For lost media, conceal each missing frame with `decode(&[])` and let
  libopus fade it. Cap concealment near 100 ms and fill the rest with
  silence, so a longer gap doesn't ring on. Confirm the cap by
  measuring distance from the original, not by ear.
- Keep the warmup and decoder-boundary rules for the packet after the gap
  intact. Test with a mocked clock.

Public API: none (behavior change). Wire: none.

## Related

- [Audio groups span 100 ms by default](/quest/m1/audio-group-200ms.md) - a lost group is now 100 ms
