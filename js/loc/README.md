# @moq/loc

Encode and decode frames for the [Low Overhead Container (LOC)](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/), a lightweight container format defined by the MoQ working group.

Each LOC frame is a small property block (timestamp, optional per-frame timescale) followed by a raw codec bitstream payload. This package provides `Format` (decoder) and `Producer` (encoder) classes that plug into `@moq/hang`'s container dispatch.

## Installation

```bash
npm add @moq/loc
```

## Usage

```typescript
import * as Loc from "@moq/loc";

// Decode incoming LOC frames
const format = new Loc.Format();
const frames = format.decode(rawFrameBytes);

// Encode outgoing LOC frames. A keyframe starts a group.
const producer = new Loc.Producer(track);
producer.encode(payload, timestampMicros, keyframe);
```

The producer is for video tracks.
An ordered group ends with an empty duration frame, at the next keyframe's timestamp or one interval after the last sample when the track closes.
`Format.end()` identifies that frame for audio and video, and the hang consumer skips it.

## License

MIT OR Apache-2.0
