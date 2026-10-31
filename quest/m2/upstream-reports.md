# [S] Report upstream bugs found during the pre-cut run

## Goal

Each bug below is reported upstream, or confirmed already fixed there, and
our workaround links the report.

## Plan

Posting outside `moq-dev` and `kixelated` needs maintainer approval for each
report.

- **GStreamer `faad`:** it drops the first AAC frame and shifts timestamps by
  one frame, so the interop AAC phase check skips GStreamer
  ([moq-dev/moq#6742](https://github.com/moq-dev/moq/pull/6742)). Remove the
  skip once it's fixed.
- **retina 0.4.21:** when the server sends an RTSP request during playback,
  retina logs a warning, returns `Pending`, and goes idle until something else
  wakes it ([moq-dev/moq#6736](https://github.com/moq-dev/moq/pull/6736)).
- **mr-boxington:** the eviction found while landing
  [moq-dev/moq#6709](https://github.com/moq-dev/moq/pull/6709).

Added 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run). Upstream reports: ✅ Chromium VideoFrame leak
([moq-dev/moq#7255](https://github.com/moq-dev/moq/pull/7255)), ✅ Firefox B-frames realtime ([moq-dev/moq#7402](https://github.com/moq-dev/moq/pull/7402)), ✅ Biome
useMathMinMax bigint ([moq-dev/moq#7305](https://github.com/moq-dev/moq/pull/7305)) approved to file; str0m/dimpl DTLS
budget ([moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379)) NOT approved.

Approved to file (search for an existing report first):

- **Chromium:** tearing down a `MediaStreamTrackProcessor` with a frame
  still in flight inside it drops that `VideoFrame` unclosed, so Chromium
  warns about an unclosed frame when `@moq/publish` capture stops, though
  the publisher closes every frame it can see. A minimal repro is ready
  from #7255.
- **Firefox (Bugzilla):** the WebCodecs video encoder emits B-frames under
  `latencyMode: "realtime"`, found in #7402, which skips such an encoder
  for the next codec.
- **Biome:** `useMathMinMax` flags comparisons of `bigint` operands, and
  its autofix to `Math.min`/`Math.max` throws `TypeError` on bigints. #7305
  suppressed the 9 sites in `js/qmux` and `js/net/src/hop.ts`; remove those
  `biome-ignore`s once it is fixed. The string and `Date` cases were fixed
  upstream ([biomejs/biome#12624](https://github.com/biomejs/biome/issues/12624),
  [biomejs/biome#10272](https://github.com/biomejs/biome/issues/10272)).

Awaiting approval, not to be filed yet:

- **str0m/dimpl:** the DTLS retry budget runs out before ICE picks a
  path, found in #7379.
  Ask again with the repro when this quest is next worked.

Public API: none. Wire: none.
