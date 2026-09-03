# [M] On-demand keyframe trigger

## Goal

An application publishing through the built-in capture path can ask for a
keyframe. Every encoder backend already forces an IDR and it is tested, but
nothing above the backend can reach it.

## Plan

`Encoder::encode` takes `keyframe: bool` and each backend honors it (NVENC via
the `FORCEIDR` picture flag with `repeatSPSPPS` so the IDR carries its
parameter sets, deliberately not `pictureType` which `enablePTD` ignores;
openh264, VAAPI, VideoToolbox and Media Foundation the same way). What is
missing is a caller-facing trigger:

- `publish_capture` forces a keyframe on the first frame and otherwise rides
  the backend's GOP cadence, with no way in.
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

- [GOP overhead](/quest/m3/gop-overhead.md) - whether a long GOP driven by a
  keyframe request is worth designing at all
