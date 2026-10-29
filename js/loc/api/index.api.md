# `@moq/loc`

```ts
import { Group } from "@moq/net";
import { Micro } from "@moq/clock";
import { Time } from "@moq/net";

export class Format {
	constructor(kind?: "audio" | "video" | "data");
	decode(frame: Uint8Array): Frame[];
	end(frame: Frame): Micro | undefined;
	readonly kind: "audio" | "video" | "data";
}

export interface Frame {
	config?: Uint8Array;
	keyframe: boolean;
	payload: Uint8Array;
	timestamp: Micro;
}

export class ObjectFormat {
	constructor(kind?: "audio" | "video" | "data");
	decode(payload: Uint8Array, timestamp?: Time.Timestamp, properties?: Group.ObjectProperties): Frame[];
	end(frame: Frame): Micro | undefined;
	readonly kind: "audio" | "video" | "data";
}
```
