# SEI sidecars

## Goal

Hang catalogs expose a top-level `sei` section alongside video. Import can lift
H.264/H.265 SEI out of an access unit into independent tracks, and supported
exporters and players can stitch it back when needed.

SEI is a section, not `kind: "sei"` on a media rendition. Separation is staged:
define the section and teach every supported consumer before import strips SEI
by default. There is no permanent mode that publishes the same SEI both inside
video and beside it.

## Plan

The sidecar relation uses media identity, not timestamp equality: video
rendition, group sequence, and frame ordinal identify the access unit. SEI must
not extend the video's existing release deadline. Publishing one track first is
not a delivery guarantee across independently scheduled streams, so
[delivery](/quest/m3/sei-delivery.md) must prove a nonblocking cross-track
mechanism before import strips SEI. If it cannot, default separation is a no-go
and SEI stays in the video access unit.

Preserve the original SEI NAL bytes, codec, prefix or suffix placement, and
in-access-unit order so stitching is byte-faithful without interpreting every
payload type. A later semantic decoder can expose captions, HDR, telemetry, or
vendor data without changing the transport contract.

The mobile (moq-kit) interoperability half and the deployment/migration half
(pinning released readers, giving legacy broadcasts an in-band path, selecting
the new profile for new broadcasts) stay downstream in moq.pro.

## Quests

- [SEI section](/quest/m2/sei/sei.md) - define the sidecar catalog, framing,
  correlation, and presence contract
- [Versioned SEI profile](/quest/m2/sei/sei-profile.md) - make an incompatible
  consumer fail before receiving video without separated metadata
- [Rust SEI split and stitch](/quest/m2/sei/sei-rust.md) - Rust importers and
  exporters round-trip sidecar SEI without delaying video
- [Web SEI stitch](/quest/m2/sei/sei-web.md) - browser consumers attach
  already-available SEI before decode and expose raw sidecar access
- [Default SEI separation implementation](/quest/m2/sei/sei-default.md) - make
  the separated profile strip by default without deploying it

## Related

- [Nonblocking SEI delivery](/quest/m3/sei-delivery.md) - the prototype that
  proves sidecars can reach a live stitcher before video without relying on
  publish order or extending the video release deadline
- [H.265 suffix SEI](/quest/m0/h265-suffix.md) - suffix SEI stays with the
  access unit it follows instead of moving to the next frame or disappearing
  at EOF; sei-rust requires it
- [SRT metadata parity](/quest/m2/srt-metadata.md) - independently preserves
  existing generic MPEG-TS metadata through the SRT gateway
- [ID3 section](/quest/m2/id3.md) - the independent typed timed-metadata section
- [Cross-track correlation](/quest/m3/teleop/correlation.md) - correlates
  application events across tracks; SEI uses exact access-unit identity instead
