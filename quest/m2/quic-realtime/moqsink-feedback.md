# [M] moqsink encoder feedback

## Goal

`moqsink` retunes the upstream encoder in its pipeline to the congestion
controller's target rate, so a GStreamer publisher sheds bitrate when uplink
capacity drops instead of queueing media behind the pacer.

## Plan

Decided 2026-10-17: its own quest in the realtime epic, independent of the
controller bake-off. It works with today's BBR3 estimate and gains the full
latency win once `Realtime` lands.

- Today `moqsink` only exposes a read-only `estimated-send-rate` property
  (`rs/moq-gst/src/sink/imp.rs`) and never touches the encoder.
- Find the encoder upstream of each sink pad and set its bitrate from the
  estimate, the way moq-cli and moq-ffi call `set_bitrate`. Settle the
  mechanism here: a known per-element bitrate property (x264enc, nvh264enc,
  vaapih264enc, and others with different units), or an upstream custom event a
  wrapper bin handles. Fail loud on an encoder it cannot retune rather than
  silently publish at a fixed rate.
- Split the estimate across sink pads the way the bandwidth `Allocator`
  does, so audio keeps its share.
- Document the behavior in `doc/bin/gstreamer.md`; tests drive it with mocked
  time.

## Related

- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - the controller whose rate this follows
- [Opus follows its grant](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the same feedback for moq-audio
