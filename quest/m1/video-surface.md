# [XS] FFI decoded frames expose a surface, only where one exists

## Goal

moq-ffi names the decoder's retained picture the way moq-video does, and a
caller cannot opt into it on a platform that has none. On `dev`
([#4094](https://github.com/moq-dev/moq/pull/4094)) the frame's view is
`MoqVideoNative` from `native()`, enabled by `MoqVideoDecoderOutput.native`.
Only macOS has a variant (`PixelBuffer`). Elsewhere the opt-in still decodes
to native surfaces, `native()` always returns `None`, and a tiled VAAPI
DMA-BUF also fails `pixels()`, so the caller gets frames it cannot read.

## Plan

Decided:

- Rename to surface naming, mirroring `moq_video::Surface`:
  `MoqVideoSurface`, `surface()`, and the matching `MoqVideoDecoderOutput`
  flag. "Native" named today's implementation, not the role.
- Refuse the opt-in at subscribe time on platforms with no surface variant
  (Windows and Linux today) with a clear unsupported error. Each platform
  lifts the refusal when its variant lands with hardware proof, in
  [decode-windows](/quest/m1/obs-moq-video/decode-windows.md) and
  [decode-linux](/quest/m1/obs-moq-video/decode-linux.md).

Guidance:

- Whether `moq_video::Output::Native` should follow the rename is a
  separate call; ask before touching it.
- Update the wrappers and docs per the root `AGENTS.md` sync table: the Go,
  Python, Swift, and Kotlin option docs mention the native surface flag, and
  `quest/m1/obs-moq-video/source.md` and the release plan on `dev` name
  `native`.
- A `dev` break on top of #4094, so it rides the same release.
- Test: the opt-in is refused where there is no variant, and on macOS
  `surface()` returns the pixel buffer.
