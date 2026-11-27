# moq-opus

[libopus](https://github.com/xiph/opus) 1.3.1, transpiled from C to unsafe Rust
with c2rust, vendored for the MoQ workspace. `moq-audio` uses it to encode and
decode Opus without a C toolchain, which keeps cross-compiling the language
bindings simple.

This is a fork of [`unsafe-libopus`](https://github.com/DCNick3/unsafe-libopus)
0.2.0 (BSD-3-Clause, like libopus). Upstream's master has since dropped the
multistream API that `moq-audio` decodes surround with, and has published no
release since 2024-11.

Its public API is a small safe wrapper over what `moq-audio` calls: `Encoder`,
`Decoder` with its `Mapping`, and the `packet` helpers. The
transpiled C functions stay private, so a safe implementation can replace them
without breaking this API.

## Changes from upstream

- SILK packet loss concealment and comfort noise no longer saturate to full
  scale. c2rust expanded `silk_SAT16`'s lower bound, `(opus_int16)0x8000`
  (-32768), into the literal `0x8000` (+32768), so every concealed or comfort
  noise sample clamped to +32767. That broke concealment of SILK and hybrid
  packets and every SILK DTX packet, which libopus decodes as a loss. The six
  inline clamps in `silk/PLC.rs` and `silk/CNG.rs` now use the lower bound
  `-0x8000` (upstream's [#3](https://github.com/DCNick3/unsafe-libopus/pull/3) fixes the
  three in `PLC.rs` on master).
- Upstream's tests, which link C libopus, are not carried; `moq-audio` tests
  the decoder it uses.
- No pointer subtraction across allocations. c2rust turned libopus's
  `OPUS_COPY`/`OPUS_MOVE` type check, `0*((dst)-(src))`, into
  `0 * dst.offset_from(src)`, which is undefined behavior when the two point
  into different buffers, even multiplied by zero. All 66 of those terms are
  removed; the copies' byte counts are unchanged.
- The C API is private: the crate-root exports, `externs`, `varargs`, and the
  exported `*_ctl!` macros are crate-only, and a safe wrapper is public instead.
- Formatted with the workspace's rustfmt, and the transpiled modules built with
  every lint allowed.

## Upstream

Vendored from the crates.io release of v0.2.0
([`3187faf`](https://github.com/DCNick3/unsafe-libopus/commit/3187faf19e11ad32916d1d850e93f2cb5134ec46)).
Last checked: master at
[`a84a3ba`](https://github.com/DCNick3/unsafe-libopus/commit/a84a3bab86cec409b2e08792a8bbee371e21b8a1)
(2024-07-08), 62 commits of refactoring past v0.2.0 that remove the
multistream API; nothing there to port beyond the concealment fix above.
