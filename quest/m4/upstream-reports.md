# [S] Report approved upstream bugs

## Goal

Each bug below is reported upstream, or confirmed already fixed there, and
our workaround links the report.

## Plan

Decided 2026-11-15 (maintainer, /quest-complete): ✅ (b) drop the three
reports that had no recorded approval, the GStreamer `faad` first-frame drop
([moq-dev/moq#6742](https://github.com/moq-dev/moq/pull/6742)), retina
0.4.21's idle `Pending`
([moq-dev/moq#6736](https://github.com/moq-dev/moq/pull/6736)), and the
mr-boxington eviction ([moq-dev/moq#6709](https://github.com/moq-dev/moq/pull/6709)),
and keep only the approved reports below (rejected: (a) approve some or all).
Each workaround already works and none blocks other work.

Added 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run). Upstream reports: ✅ Chromium VideoFrame leak
([moq-dev/moq#7255](https://github.com/moq-dev/moq/pull/7255)), ✅ Firefox
B-frames realtime ([moq-dev/moq#7402](https://github.com/moq-dev/moq/pull/7402)),
✅ Biome useMathMinMax bigint
([moq-dev/moq#7305](https://github.com/moq-dev/moq/pull/7305)) approved to
file; str0m/dimpl DTLS budget
([moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379)) NOT approved.

Approved to file, each after searching for an existing report and reading
the tracker's policy on LLM-written reports; where it bans them, hand the
maintainer a draft instead:

- **Chromium:** tearing down a `MediaStreamTrackProcessor` with a frame
  still in flight inside it drops that `VideoFrame` unclosed, so Chromium
  warns about an unclosed frame when `@moq/publish` capture stops, though
  the publisher closes every frame it can see. A minimal repro is ready
  from #7255.
- **Firefox (Bugzilla):** the WebCodecs video encoder emits B-frames under
  `latencyMode: "realtime"`, found in #7402, which skips such an encoder
  for the next codec.
- **Biome:** filed 2026-10-31 as
  [biomejs/biome#12796](https://github.com/biomejs/biome/issues/12796)
  (open, checked 2026-11-12): `useMathMinMax` flags comparisons of `bigint`
  operands, and its autofix to `Math.min`/`Math.max` throws `TypeError` on
  bigints. #7305 suppressed the sites in `js/qmux` and `js/net/src/hop.ts`;
  link the report from their 8 `biome-ignore` comments, then remove them once
  it is fixed. The string and `Date` cases were fixed
  upstream ([biomejs/biome#12624](https://github.com/biomejs/biome/issues/12624),
  [biomejs/biome#10272](https://github.com/biomejs/biome/issues/10272)).

Not to be filed:

- **str0m/dimpl:** the DTLS retry budget runs out before ICE picks a
  path, found in #7379. Not filed while the
  [WebRTC epic](/quest/m3/webrtc/README.md) vendored str0m. Since
  2026-11-16 moq-webrtc [returns to upstream](/quest/m2/str0m-upstream.md),
  so this and three more become candidates once their quests show the
  public API cannot do it: [DTLS waits for ICE](/quest/m3/webrtc/dtls-after-ice.md),
  [the typed DTLS timeout](/quest/m3/webrtc/dtls-timeout.md),
  [several bandwidth lines](/quest/m4/webrtc-sdp-bandwidth.md), and the
  NACK hook of [WHEP pacing](/quest/m3/rtc-pacing.md). Each needs the
  maintainer's approval and a read of the repository's AI policy before it
  is filed.

Public API: none. Wire: none.
