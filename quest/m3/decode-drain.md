# [M] Drain pipelined decoders at group and track boundaries

## Goal

A decoder that holds pictures back (NVDEC, the V4L2 stateful decoder) hands
every one of them over before a track ends, and a transcode's output groups
begin with the picture that began the input group rather than a straggler
from the one before.

## Plan

`decode::Backend` has `decode` and `name` and nothing else: there is no way to
tell a backend the stream is pausing or over. A one-in-one-out decoder
(openh264, VideoToolbox) never needs one, but NVDEC and V4L2 both pipeline, so
the last few pictures of a finite stream stay inside the driver, and at a
group boundary in `moq-transcode` the encoder is flushed while the decoder is
not, so pictures from the closing group surface after the next group's first
access unit went in and are encoded ahead of its keyframe.

Add `flush` and `finish` to `decode::Backend`, mirroring the encode trait and
with the same rule: no default implementation, so a pipelined backend cannot
inherit a no-op and look like it worked. The V4L2 implementation is the
kernel's drain sequence (`V4L2_DEC_CMD_STOP`, dequeue CAPTURE through
`V4L2_BUF_FLAG_LAST`, `V4L2_DEC_CMD_START`), with a source change that lands
mid-drain handled the way `drain_tail` already handles one. NVDEC's is an
end-of-stream packet through the parser. Call `flush` where `moq-transcode`
flushes the encoder and `finish` before a consumer reports the track over, and
cover it with a delayed-backend test that proves no picture is lost or crosses
a group boundary.

## Related

- [Embedded video](/quest/m3/video-embedded.md) - the V4L2 decoder this drains
