# [S] One default cap formula

## Goal

Rust (`moq-video`, and so moq-ffi, moq-c, and moq-cli), the JS publisher,
and moq-transcode's default ladder derive a rendition's default cap from one
formula, and the hang draft defines the catalog `bitrate` as that peak.
720p30 H.264 caps at 4 Mbps instead of today's 1.94 Mbps.

## Plan

Decided with the maintainer on 2026-11-15 (/quest-plan):

- **Anchors.** 720p30 H.264 = 4 Mbps and 720p60 = 6 Mbps, close to Twitch's
  guidelines plus a third (rejected: 720p30 = 6 Mbps; 1080p30 = 11 Mbps,
  "waaay too high").
- **Formula.** ✅ `7.2 Mbps × Mpx / (1 + Mpx / 1.38)`, which levels off
  toward 10 Mbps (rejected: `4 Mbps × (pixels / 720p)^0.75`, which prices
  1080p30 at 7.35 and 1080p60 at 11; a square-root law, which prices 360p at
  2 Mbps and shuts out weak links). At 30 fps H.264: 240p 0.69, 360p 1.4,
  480p 2.3, 720p 4.0, 1080p 6.0, 1440p 7.3, 4K 8.6 Mbps. 4K is low for H.264;
  4K publishers usually pick H.265 or AV1, or set a cap.
- **Frame rate and codec**, as `js/publish/src/video/encoder.ts` `ceiling()`
  already does: multiply by `(30 + (fps - 30) / 2) / 30`, and by the codec
  scale (avc1 1.0, hev1 0.7, vp09 0.8, av01 0.6, vp8 1.1). Rust's default
  (`rs/moq-video/src/encode/encoder.rs`) is linear in frame rate and ignores
  codec today; it adopts both.
- **Transcode ladder.** The fixed rungs (240p 350k to 1080p 5M,
  `rs/moq-transcode/src/ladder.rs`) derive from the formula; `--rung size:bps`
  still overrides.
- **Catalog.** `rs/hang` and `js/hang` already document `bitrate` as the
  maximum; the hang draft gives media renditions no definition, so define it
  there as the peak the encoder is held to.

moq-boy's default changes with the formula (about 97 kbps today); its
[quality quest](/quest/m9/video-quality/moq-boy.md) measures what it needs.

Public API: none (defaults and docs). Wire: none.

## Related

- [moq-boy encodes cleanly](/quest/m9/video-quality/moq-boy.md) - its default bitrate moves with this formula
