# `@moq/hang/cmaf`

```ts
import { Frame } from "@moq/hang/container";
import { Net } from "@moq/hang";

export class Format implements import("@moq/hang/container").Format {
	constructor(init: InitSegment);
	decode(payload: Uint8Array, timestamp: Net.Time.Timestamp | undefined): Frame[];
}

export interface InitSegment {
	defaultSampleDuration: number;
	defaultSampleFlags: number;
	defaultSampleSize: number;
	description?: Uint8Array<ArrayBufferLike>;
	kind: "video" | "audio";
	timescale: number;
	trackId: number;
}

export interface Sample {
	data: Uint8Array;
	duration: number;
	keyframe: boolean;
	timestamp: number;
}

export function decodeDataSegment(segment: Uint8Array, init: InitSegment, timestamp: Net.Time.Timestamp | undefined): Sample[];

export function decodeInitSegment(init: Uint8Array): InitSegment;
```
