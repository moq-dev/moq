# `@moq/hang/cmaf`

```ts
import { Format as Format_1 } from "@moq/hang/container";
import { Frame } from "@moq/hang/container";
import { Net } from "@moq/hang";

export class Format implements Format_1 {
	constructor(init: InitSegment);
	decode(payload: Uint8Array, timestamp: Net.Time.Timestamp | undefined): Frame[];
}

export interface InitSegment {
	defaultSampleDuration: number;
	defaultSampleFlags: number;
	defaultSampleSize: number;
	description?: Uint8Array;
	kind: "audio" | "video";
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
