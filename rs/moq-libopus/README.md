# moq-libopus

[libopus](https://github.com/xiph/opus) 1.3.1, transpiled from C to unsafe Rust
with c2rust, vendored for the MoQ workspace. `moq-audio` uses it to encode and
decode Opus without a C toolchain, which keeps cross-compiling the language
bindings simple.

This is a fork of [`unsafe-libopus`](https://github.com/DCNick3/unsafe-libopus)
0.2.0 (BSD-3-Clause, like libopus). The API is unchanged: the C functions and
constants under their C names. Upstream's master has since dropped the
multistream API that `moq-audio` decodes surround with, and has published no
release since 2024-11.

## Changes from upstream

- SILK packet loss concealment and comfort noise no longer saturate to full
  scale. c2rust expanded `silk_SAT16`'s lower bound, `(opus_int16)0x8000`
  (-32768), into the literal `0x8000` (+32768), so every concealed or comfort
  noise sample clamped to +32767. That broke concealment of SILK and hybrid
  packets and every SILK DTX packet, which libopus decodes as a loss. The six
  expansions in `silk/PLC.rs` and `silk/CNG.rs` now call `silk_SAT16`
  (upstream's [#3](https://github.com/DCNick3/unsafe-libopus/pull/3) fixes the
  three in `PLC.rs` on master).
- Upstream's tests, which link C libopus, are not carried; `moq-audio` tests
  the decoder it uses.
