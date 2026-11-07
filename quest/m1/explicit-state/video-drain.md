# [M] Video codec drains

## Goal

When a moq-video decoder's or encoder's flush or drain fails, for example a
drain that times out, the call returns with the codec either ready for input
again or terminally failed. It is never left in end-of-stream or stopped
while the front end keeps feeding it. A decoder opens its keyframe gate only
once a keyframe has actually decoded. This holds for MediaCodec and V4L2
decode and encode, and for every other backend with the same drain shape.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

Real MediaCodec and V4L2 devices aren't in CI, so reproducing these may need
a seam the backends drive the codec through. The `vicodec`/`visl` virtual
V4L2 drivers are worth checking for the V4L2 half.

The front end retries a failed read on purpose
(`rs/moq-video/src/decode/consumer.rs`: "Left idle, so the next read asks it
again"). That retry turns each hole below into a stuck codec:

- **MediaCodec decode** (`decode/backend/mediacodec.rs`):
  - `flush` returns at `drain_tail()?` before it reaches
    `if self.fed { codec.flush() }`, so a drain that never sees EOS leaves the
    codec in end-of-stream.
  - `fed: bool` can't tell "fed" from "EOS already signalled". The next
    `flush` signals EOS again instead of flushing, so the decoder stays stuck.
  - Recommended: `enum Input { Idle, Fed, EndSignalled }`, flushing the codec
    whenever it is `EndSignalled`.
- **V4L2 decode** (`decode/backend/v4l2.rs`): `V4L2_DEC_CMD_STOP` followed by
  a failed `drain_sequence` skips `V4L2_DEC_CMD_START`, so the decoder stays
  stopped and nothing records it.
- **V4L2 encode** (`encode/backend/v4l2.rs`):
  - It has the same STOP/START hole.
  - `start()` can leave the raw queue streaming with the coded queue off.
    After that, `encode` never retries and `drain_tail` returns empty.
- **MediaCodec encode** (`encode/backend/mediacodec.rs`): `ended` is set
  before the drain loop, and `encode` never checks it.
- **Keyframe gate** (`decode/decoder.rs`):
  - `got_keyframe = true` is set before `backend.decode(...)?`.
  - A keyframe that fails to decode still opens the gate, and the deltas that
    follow reach a backend with no reference.

Every caller ends the stream on an encoder error today, so the encoder holes
are latent. Fix them with the same state anyway rather than leave the twins
different.

Also sweep:

- `encode/backend/mediafoundation.rs`: five bools.
- `decode/backend/videotoolbox.rs`: three `Option`s kept in lockstep, and a
  rebuild drops the old session without draining it.
- `encode/backend/v4l2.rs` and `rs/moq-v4l/src/io/*/stream.rs`: `active` plus
  `held`, where a half-start leaves buffers queued.
- `vaapi.rs` and `vpx.rs`: the small flag pairs.

[moq-dev/moq#8013](https://github.com/moq-dev/moq/pull/8013) (VAAPI) and
[moq-dev/moq#7916](https://github.com/moq-dev/moq/pull/7916) also edit
`decode/decoder.rs`, so expect merge conflicts.

## Related

- [VAAPI encode and decode](/quest/m1/video-vaapi.md) - edits the same decoder front end
