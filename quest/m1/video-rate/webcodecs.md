# [L] Capped VBR in the browser publisher

## Goal

`js/publish` encodes constant quality under a hard peak at the rendition's
cap: it uses WebCodecs `bitrateMode: "quantizer"` with its own per-frame QP
control where the browser supports it, and `"constant"` at the cap
everywhere else. Today it leaves WebCodecs on its `"variable"` default, an
average target with no peak. The PR measures the average against the cap in
Chrome on static, talking head, and high-motion clips.

## Plan

Decided with the maintainer on 2026-11-15 (/quest-plan): ✅ quantizer where
supported, otherwise `"constant"`, since the encoder must never go over the
cap (rejected: `"variable"` at the cap, which has no peak; `"variable"` at
half the cap, which still has no peak and spends its average on calm scenes).

- Decide the mode at setup with `VideoEncoder.isConfigSupported` for the
  chosen codec, not by trying at runtime.
- The controller holds a quality QP and raises it when the rolling output rate
  nears `min(cap, grant)`, then relaxes it back. It sees each frame's size
  only after encoding, so the window and step must keep a burst under the cap;
  pick them by measurement.
- Per-frame QP goes in the codec-specific encode options (`avc`, `hevc`,
  `vp9`, `av1` `quantizer`); check each codec's QP range.
- `"constant"` keeps today's reconfigure on grant changes.

Public API: none. Wire: none.

## Required

- [One default cap formula](/quest/m1/video-rate/cap-formula.md) - the cap this controller holds; both touch `ceiling()` in `js/publish/src/video/encoder.ts`
