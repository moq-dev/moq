# `@moq/hang/locmaf`

```ts
import { Format as Format_1 } from "@moq/hang/container";
import { Frame } from "@moq/hang/container";
import { InitSegment } from "@moq/hang/cmaf";
import { Net } from "@moq/hang";

export class Format implements Format_1 {
	constructor(init: InitSegment);
	decode(payload: Uint8Array, timestamp: Net.Time.Timestamp | undefined): Frame[];
	group(): Format;
}
```
