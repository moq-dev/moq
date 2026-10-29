# @moq/loc

Encode and decode frames for the [Low Overhead Container (LOC)](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/), a lightweight container format defined by the MoQ working group.

Two framings plug into `@moq/hang`'s container dispatch, chosen by the catalog:

- `Format`, hang's `loc` container: each frame is a small property block (timestamp, optional per-frame timescale) followed by the codec bitstream. It works on every transport.
- `ObjectFormat`, the draft's own framing (hang `ietf-loc`, MSF `loc`): the properties are the moq-transport Object Properties and the payload is the bare codec bitstream. It refuses an object without them, as moq-lite delivers it.

## Installation

```bash
npm add @moq/loc
```

## Usage

```typescript
import * as Loc from "@moq/loc";

// Decode incoming LOC frames
const format = new Loc.Format("video");
const frames = format.decode(rawFrameBytes);

// Decode a LOC-04 object from a received moq-net frame
const object = new Loc.ObjectFormat("video");
const [frame] = object.decode(received.payload, received.timestamp, received.properties);
```

An audio or video group may end with an empty duration frame at the exclusive end of its last sample.
`Format.end()` identifies that frame, and the hang consumer skips it.

## License

MIT OR Apache-2.0
