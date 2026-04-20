# @moq/msf

Zod schemas and helpers for the [MSF (MOQT Streaming Format)](https://datatracker.ietf.org/doc/draft-ietf-moq-streaming-format/) catalog.

This package provides types for decoding the MSF catalog track delivered over `moq-lite`. It is consumed by `@moq/watch` when the player is configured with `catalogFormat: "msf"`.

## Installation

```bash
npm add @moq/msf
```

## Usage

```typescript
import * as Msf from "@moq/msf";

// Fetch and decode a catalog from a moq-lite track
const catalog = await Msf.fetch(track);
if (catalog) {
	for (const t of catalog.tracks) {
		console.log(t.name, t.role, t.codec);
	}
}
```

## License

MIT OR Apache-2.0
