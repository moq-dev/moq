# [M] Keep the browser publisher under the cap without a per-frame quantizer

## Goal

`js/publish` never sends more than the rendition's cap on a browser and codec
that lack an honored per-frame quantizer. `"constant"` doesn't hold the cap in
practice. Measured in Chrome 156 on 720p30 clips (Xiph `park_joy`, cut to a
static frame for the scene-cut case) during
[moq-dev/moq#9026](https://github.com/moq-dev/moq/pull/9026):

- Chrome's software H.264 (OpenH264, the codec Linux and Windows Chrome pick
  without a hardware encoder) produces identical output for `"constant"` and
  `"variable"`. On high motion it averages 1.13x the cap, and at a 1 Mbps grant
  3.55x, with 25 s queued at the grant.
- Chrome's software VP9, AV1, and VP8 in `"constant"` reach 2.1 to 3.3x a
  1 Mbps grant on high motion: their quantizer range stops short.
- macOS VideoToolbox H.264 in `"constant"` at 1 Mbps drops 37% of frames and
  still peaks at 2.1x over a second.
- Firefox ignores the per-frame quantizer, so it always runs `"constant"`.

The quantizer path holds 1.00x with at most 0.3 s queued in all of these
cases, short of a scene cut.

## Plan

Options:

- Within each probe pass, rank codecs whose encoder honors a per-frame
  quantizer ahead of those that don't, so Chrome without a hardware encoder
  picks software VP9 over software H.264. Measured cost at 720p on the fedora
  host: VP9 takes 5.5 ms per frame on the talking head and 12.7 ms on high
  motion, against H.264's 3.5 and 5.7 ms. VP9 is already first in the hardware
  pass, so viewers already decode it.
- Wrap `"constant"` in an outer loop over the measured output: lower the
  configured bitrate while output overshoots, and skip frames while the
  backlog passes the window. This covers Firefox and VideoToolbox too, but each
  reconfigure reinitializes OpenH264.

## Waiting

- Maintainer: pick the approach. Recommended: rank quantizer codecs first,
  since it also gains constant quality (talking head at 22% of the cap instead
  of 99%). Add the outer loop only if Firefox's overshoot measures as a problem.
  An agent can't make this call: it trades publisher CPU against the cap.
