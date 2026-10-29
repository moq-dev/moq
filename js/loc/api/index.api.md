# `@moq/loc`

```ts
import { Micro } from "@moq/clock";

export class Format {
	constructor(kind?: "audio" | "video" | "data");
	decode(frame: Uint8Array): Frame[];
	end(frame: Frame): Micro | undefined;
	readonly kind: "audio" | "video" | "data";
}

export interface Frame {
	keyframe: boolean;
	payload: Uint8Array;
	timestamp: Micro;
}
```
