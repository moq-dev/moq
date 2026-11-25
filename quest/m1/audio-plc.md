# [S] Native audio conceals SILK and hybrid Opus losses

## Goal

When groups are lost on a SILK or hybrid Opus track (speech, the `Voice`
encoder kind, low bitrates), `moq_audio::decode::Consumer` fills the gap with
packet loss concealment (PLC) instead of a silent `Read::Gap`, as it already
does for CELT-only Opus.

Not here: `@moq/watch`, since WebCodecs' `AudioDecoder` has no PLC or FEC
input; Opus in-band FEC (LBRR) and DRED.

## Plan

CELT-only Opus already conceals: the consumer decodes each lost packet with
`decode(&[])`, up to `CONCEAL_MAX` (100 ms), and reads only the rest as a gap.
A declared end is never concealed. See `conceal` in
`rs/moq-audio/src/decode/consumer.rs`.

SILK and hybrid were left out because the bundled decoder, unsafe-libopus
0.2.0, conceals them at full scale: c2rust turned `silk_SAT16`'s lower bound
`(opus_int16)0x8000` into `+0x8000`, so every concealed sample clamps to
+32767. The bug sits in three sites in `silk/PLC.rs` and three in `silk/CNG.rs`.
C libopus 1.6.1 conceals the same packets normally, fading to -40 dB within
about 80 ms. The fix for master is
[DCNick3/unsafe-libopus#3](https://github.com/DCNick3/unsafe-libopus/pull/3), open
since 2026-07 with no release since 2024-11. Until a fixed decoder lands,
`Libopus` refuses `decode(&[])` after a SILK or hybrid packet, and
`Backend::conceals` reports it.

Once the decoder conceals SILK correctly, delete the `celt` gate in
`rs/moq-audio/src/decode/backend/libopus.rs` and `opus::celt_only`. Then flip
`opus_silk_loss_stays_a_gap` and `opus_refuses_to_conceal_after_silk` to
expect concealment that decays rather than saturates.

## Waiting

- Maintainer: choose how moq-audio gets a SILK PLC fix. Patching through
  `[patch.crates-io]` doesn't reach crates.io users of moq-audio. The options
  are (a) a moq-dev fork of unsafe-libopus 0.2.0 with the six-site fix,
  published under a new crate name, (b) asking upstream to merge PR #3 and cut
  0.2.1, which needs approval to post outside our orgs, or (c) decoding Opus
  through C libopus. Recommended: (a), plus a comment asking upstream for
  0.2.1, so the fork goes away if they release.
