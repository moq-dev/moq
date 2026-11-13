# [S] H.265 encodes respect the encoder's minimum size

## Goal

An H.265 encode narrower than the hardware encoder's minimum never fails
with an opaque driver error. moq-video's encoder probe reports the driver's
minimum width and height, `Kind::Auto` passes over a hardware candidate
whose minimum the frame is below, and with no candidate left the error names
the minimum. The caller today is `moq import capture --codec h265`
(`CaptureArgs::codec`, `rs/moq-cli/src/publish.rs`).

## Plan

Found 2026-11-06 in [moq-dev/moq#8013](https://github.com/moq-dev/moq/pull/8013).
AMD's VCN 5 (radeonsi, Mesa `si_get.c`) does not encode H.265 narrower than
384. moq-video has no software H.265 encoder, so on an AMD-only host an
H.265 encode below that fails to open.

moq-transcode cannot hit this yet: its `Config` has no output codec, and
`catalog::rung_entry` (`rs/moq-transcode/src/catalog.rs`) and `Rung::encode`
(`rs/moq-transcode/src/rung.rs`) leave `encode::Config` at its H.264
default. Adding an H.265 output to the ladder is out of this quest's scope.
Whoever adds it applies the probe's minimum per rung (the default 240p rung
is 320 wide for a 4:3 source), and a failed rung probe must fail only that
rung: today `Pipeline::new` propagates it and aborts the whole transcode.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ an S quest in m3. Not
chosen: folding it into the radeonsi quest (since completed), or dropping it.

Decided 2026-11-07 (maintainer, /quest-complete): ✅ narrowed to the
capture encode (`moq import capture --codec h265`), an S in m3, with H.265
transcode output out of scope. Not chosen: making H.265 transcode output a
prerequisite inside this quest.

- Learn the minimum rather than hard-coding 384. libva has no minimum
  picture-size attribute (only `VAConfigAttribMaxPictureWidth`/`Height`), so
  it comes from the failed open or a per-driver table. Check what NVENC and
  VideoToolbox report for the same case.

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): the probe stays private, and the typed error names the minimum width. It becomes public only when a consumer asks.

Public API: the encode error names the minimum size; the probe stays private. Wire: none.

## Related

- [Vulkan Video encode on AMD](/quest/m5/vulkan-encode.md) - may report a different minimum
