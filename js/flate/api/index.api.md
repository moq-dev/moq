# `@moq/flate`

```ts
import { Timed } from "@moq/net";
import { Track } from "@moq/net";

export type Compression = "none" | "deflate";

export const DEFAULT_LEVEL: 6;

export class Decoder {
	constructor();
	frame(slice: Uint8Array): Uint8Array;
}

export class Encoder {
	constructor(options?: EncoderOptions);
	static bound(len: number): number;
	frame(payload: Uint8Array): Uint8Array;
}

export interface EncoderOptions {
	level?: number;
}

export namespace Snapshot {
	export interface Config {
		compression?: Compression;
	}
	export namespace Consumer {
		export type Config = Snapshot.Config & {
			track: Track.Subscriber;
		};
	}
	export class Consumer {
		constructor(config: Snapshot.Consumer.Config);
		[asyncIterator](): AsyncIterator<Timed<Uint8Array<ArrayBufferLike>>>;
		latest(): Promise<Timed<Uint8Array<ArrayBufferLike>> | undefined>;
		next(): Promise<Timed<Uint8Array<ArrayBufferLike>> | undefined>;
	}
	export namespace Producer {
		export type Config = Snapshot.Config & {
			track: Track.Producer;
		};
	}
	export class Producer {
		constructor(config: Snapshot.Producer.Config);
		finish(): void;
		update({ ... }: Timed<Uint8Array<ArrayBufferLike>>): void;
	}
}

export namespace Stream {
	export interface Config {
		compression?: Compression;
	}
	export namespace Consumer {
		export type Config = Stream.Config & {
			track: Track.Subscriber;
		};
	}
	export class Consumer {
		constructor(config: Stream.Consumer.Config);
		[asyncIterator](): AsyncIterator<Timed<Uint8Array<ArrayBufferLike>>>;
		next(): Promise<Timed<Uint8Array<ArrayBufferLike>> | undefined>;
	}
	export namespace Producer {
		export type Config = Stream.Config & {
			track: Track.Producer;
		};
	}
	export class Producer {
		constructor(config: Stream.Producer.Config);
		append({ ... }: Timed<Uint8Array<ArrayBufferLike>>): void;
		finish(): void;
	}
	export class Rolled extends Error {
		constructor();
	}
}
```
