# @moq/loc

Encode and decode frames for the [Low Overhead Container (LOC)](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/), a lightweight container format defined by the MoQ working group.

Each LOC frame is a small property block (timestamp, optional per-frame timescale) followed by a raw codec bitstream payload. This package provides the `Format` decoder that plugs into `@moq/hang`'s container dispatch.

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
```

An audio or video group may end with an empty duration frame at the exclusive end of its last sample.
`Format.end()` identifies that frame, and the hang consumer skips it.

## License

MIT OR Apache-2.0
