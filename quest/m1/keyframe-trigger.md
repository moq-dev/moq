# [M] On-demand keyframe trigger

## Goal

An application publishing through the built-in capture path can ask for a
keyframe. `Encoder::cut()`, `Sink::cut()`, and the ffi/libmoq `cut` already
force one, refusing with `CutUnsupported` when a backend cannot, but the
turnkey capture paths have no way in.

## Plan

`Backend::encode(frame, cut)` honors a cut (NVENC via the `FORCEIDR` picture
flag with `repeatSPSPPS` so the IDR carries its parameter sets, deliberately
not `pictureType` which `enablePTD` ignores; openh264, VAAPI, VideoToolbox and
Media Foundation the same way) and `can_cut()` answers at open whether it can.
What is missing is a caller-facing trigger on the turnkey paths:

- `publish_capture` relies on every backend opening with a keyframe and
  otherwise rides the GOP cadence; its `Options` carry no trigger.
- `js/publish`'s encode path already calls `encoder.encode(frame, { keyFrame })`,
  but `lastKeyframe` is a closure-local `let` with no external trigger.
  `Config.keyframeInterval` is cadence, not on demand.

Give both a trigger the caller owns: a handle on the Rust capture path, and a
Signal the JS encode effect reads instead of its local variable. Coalesce
requests, so several arriving within one frame interval produce one IDR rather
than a run of them, and rate limit at the publisher so a caller in a loop
cannot pin the encoder at all-IDR.

Additive on both sides, so it lands on `main`. Real callers exist regardless
of whether a wire-level request ever ships: a resume, a recording cut, a
rendition switch, and an application that knows its own tune-in moment.

## Related

- [GOP overhead](/quest/m2/gop-overhead.md) - whether a long GOP driven by a
  keyframe request is worth designing at all
