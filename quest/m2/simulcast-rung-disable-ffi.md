# [M] moq-ffi video producers disable simulcast rungs the grant cannot fund

## Goal

An application publishing several `MoqVideoProducer`s of one source over
moq-ffi stops encoding its top renditions when their bandwidth grant cannot
fund them, advertising each as `enabled: false` in the catalog, and resumes
them once the grant recovers, even after their viewers left. The lower
renditions keep their share. Prove one shaped-uplink run through moq-ffi.

## Plan

The rule and its tests already live in `moq_mux::rate::Rung`, and a
reservation's grant keeps reporting an idle track's would-be share, so this
is wiring. `@moq/publish` is the reference: its video encoder takes
`lowerBitrate` and `priority` and follows a `Rate.Rung`
(`js/publish/src/video/encoder.ts`).

- Knobs on `MoqVideoEncoderOutput`, mirroring the JS names: the next lower
  rendition's ceiling, and the track priority (`PRIORITY.video` by default)
  so the allocator fills lower renditions first. Refuse a lower ceiling at or
  above this one's.
- `follow_reservation` (`rs/moq-ffi/src/video.rs`) follows a `Rung` instead
  of a `Control` when the knob is set. While disabled, `write` drops frames
  instead of encoding them; the first frame after resuming is a keyframe.
- Catalog `enabled` follows what the producer applied. `moq_video::encode::Producer`
  needs a way to set it, through the importer's `container::Producer::modify`.
- Every binding wrapper and `doc/lib/*` per the Cross-Package Sync table.

## Related

- [OBS rung disable](/quest/m2/obs-rung-disable.md) - the OBS plugin applying the same rule per encoder
- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
