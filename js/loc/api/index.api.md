# `@moq/loc`

```ts
import { Duration } from "@moq/clock";

export class Format {
	constructor(kind?: "audio" | "video" | "data");
	decode(frame: Uint8Array): Frame[];
	end(frame: Frame): Duration | undefined;
	readonly kind: "audio" | "video" | "data";
}

export interface Frame {
	keyframe: boolean;
	payload: Uint8Array;
	timestamp: Duration;
}
```
