# SEI sidecars

## Goal

Hang catalogs expose a top-level `sei` section alongside video. Import lifts
H.264/H.265 SEI out of the access unit into its own track, and export reinserts
it, the way MPEG-TS demuxes a PID on the way in and remuxes it on the way out.
A consumer can read the metadata without subscribing to video at all.

SEI is a section, not `kind: "sei"` on a media rendition. Import strips by
default: the bytes live in exactly one place, never both.

## Plan

The driving use case is metadata without video. Broadcast contribution carries
timecode, ad markers, and telemetry in SEI today, and reaching it currently
means downloading and demuxing the whole video track. A separate track means a
scoreboard overlay, a stats joiner, or a backgrounded tab can subscribe to just
the metadata.

Stripping is safe because SEI is non-normative for decoding: a decoder builds
identical pictures without it. Nothing we ship reads SEI anyway. Outside the
recovery-point check in `moq-mux`'s H.264 splitter, the whole repository has no
SEI consumer, no HDR10 handling, and no colour model. So the player never
stitches. It subscribes to the sidecar when it wants the data, and otherwise
decodes stripped video unchanged. Only export reinserts, rebuilding the
bitstream a downstream decoder expects.

That removes the delivery problem this line used to be gated on. Nothing waits
on a sidecar to release a video frame, so there is no cross-track deadline to
prove, no presence marker in the video framing, and no compatibility cliff for
an old player. An old exporter drops SEI from its output; upgrading is the
answer.

Correlate by the video frame's timestamp, which already identifies the access
unit within a rendition and is the key an application syncing data to
presentation time actually wants. Preserve the original NAL bytes, prefix or
suffix placement, and in-access-unit order so reinsertion is byte-faithful
without interpreting payload types. A semantic decoder for captions, HDR,
telemetry, or vendor data can be layered on later without changing this
contract.

## Quests

- [SEI section](/quest/m2/sei/sei.md) - define the sidecar catalog, framing,
  and correlation contract
- [Rust SEI split and reinsert](/quest/m2/sei/sei-rust.md) - importers strip SEI
  into the sidecar and exporters put it back byte-faithfully
- [Web SEI access](/quest/m2/sei/sei-web.md) - browser consumers discover and
  read the sidecar without subscribing to video

## Related

- [H.265 suffix SEI](/quest/m0/h265-suffix.md) - suffix SEI stays with the
  access unit it follows instead of moving to the next frame or disappearing
  at EOF; sei-rust requires it
- [Colour model](/quest/m2/color-model.md) - the catalog has nowhere to put
  display metadata such as HDR10 mastering display; a pre-existing gap this
  line does not widen, because no renderer here reads that SEI today
- [CEA-608/708](/quest/m2/captions-cea.md) - decodes caption SEI into a text
  rendition; a candidate to move onto this sidecar rather than walking the
  access unit itself
- [SRT metadata parity](/quest/m2/srt-metadata.md) - independently preserves
  existing generic MPEG-TS metadata through the SRT gateway
- [ID3 section](/quest/m2/id3.md) - the independent typed timed-metadata section
