# `@moq/stats`

```ts
import { $strip } from "zod/v4/core";
import { Broadcast } from "@moq/net";
import { Origin } from "@moq/net";
import { Path } from "@moq/net";
import { ZodMiniDefault } from "zod/mini";
import { ZodMiniNumber } from "zod/mini";
import { ZodMiniObject } from "zod/mini";
import { ZodMiniPipe } from "zod/mini";
import { ZodMiniPrefault } from "zod/mini";
import { ZodMiniRecord } from "zod/mini";
import { ZodMiniString } from "zod/mini";
import { ZodMiniTransform } from "zod/mini";
import { infer } from "zod/mini";

export type Bucket = infer<typeof BucketSchema>;

export type BucketFrame = infer<typeof BucketFrameSchema>;

export const BucketFrameSchema: ZodMiniObject<{
	epoch: ZodMiniString<string>;
	tiers: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
		labels: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
			received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			requests: ZodMiniDefault<ZodMiniNumber<number>>;
			routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
			sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			sessions_failed: ZodMiniPrefault<ZodMiniObject<{
				application: ZodMiniDefault<ZodMiniNumber<number>>;
				expired: ZodMiniDefault<ZodMiniNumber<number>>;
				idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
				transport: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			sessions_refused: ZodMiniPrefault<ZodMiniObject<{
				expired: ZodMiniDefault<ZodMiniNumber<number>>;
				forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
				membership: ZodMiniDefault<ZodMiniNumber<number>>;
				refused: ZodMiniDefault<ZodMiniNumber<number>>;
				request: ZodMiniDefault<ZodMiniNumber<number>>;
				unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
			wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
			wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>>>;
		paths: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
			publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
				announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
				announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
				broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
				broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
				fetches: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
				inflated: ZodMiniDefault<ZodMiniNumber<number>>;
				stale: ZodMiniPrefault<ZodMiniObject<{
					bytes: ZodMiniDefault<ZodMiniNumber<number>>;
					frames: ZodMiniDefault<ZodMiniNumber<number>>;
					groups: ZodMiniDefault<ZodMiniNumber<number>>;
				}, $strip>>;
				subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
				subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>>;
			subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
				announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
				announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
				broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
				broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
				fetches: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
				inflated: ZodMiniDefault<ZodMiniNumber<number>>;
				stale: ZodMiniPrefault<ZodMiniObject<{
					bytes: ZodMiniDefault<ZodMiniNumber<number>>;
					frames: ZodMiniDefault<ZodMiniNumber<number>>;
					groups: ZodMiniDefault<ZodMiniNumber<number>>;
				}, $strip>>;
				subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
				subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>>;
		}, $strip>>>>;
		publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		sessions: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
			received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			requests: ZodMiniDefault<ZodMiniNumber<number>>;
			routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
			sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			sessions_failed: ZodMiniPrefault<ZodMiniObject<{
				application: ZodMiniDefault<ZodMiniNumber<number>>;
				expired: ZodMiniDefault<ZodMiniNumber<number>>;
				idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
				transport: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			sessions_refused: ZodMiniPrefault<ZodMiniObject<{
				expired: ZodMiniDefault<ZodMiniNumber<number>>;
				forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
				membership: ZodMiniDefault<ZodMiniNumber<number>>;
				refused: ZodMiniDefault<ZodMiniNumber<number>>;
				request: ZodMiniDefault<ZodMiniNumber<number>>;
				unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
			wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
			wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
	}, $strip>>>>;
}, $strip>;

export class BucketReader {
	close(): void;
	next(): Promise<BucketUpdate | undefined>;
}

export const BucketSchema: ZodMiniPrefault<ZodMiniObject<{
	labels: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		requests: ZodMiniDefault<ZodMiniNumber<number>>;
		routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
		sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_failed: ZodMiniPrefault<ZodMiniObject<{
			application: ZodMiniDefault<ZodMiniNumber<number>>;
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
			transport: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_refused: ZodMiniPrefault<ZodMiniObject<{
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
			membership: ZodMiniDefault<ZodMiniNumber<number>>;
			refused: ZodMiniDefault<ZodMiniNumber<number>>;
			request: ZodMiniDefault<ZodMiniNumber<number>>;
			unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>>>;
	paths: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
		publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
	}, $strip>>>>;
	publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	sessions: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		requests: ZodMiniDefault<ZodMiniNumber<number>>;
		routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
		sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_failed: ZodMiniPrefault<ZodMiniObject<{
			application: ZodMiniDefault<ZodMiniNumber<number>>;
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
			transport: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_refused: ZodMiniPrefault<ZodMiniObject<{
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
			membership: ZodMiniDefault<ZodMiniNumber<number>>;
			refused: ZodMiniDefault<ZodMiniNumber<number>>;
			request: ZodMiniDefault<ZodMiniNumber<number>>;
			unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
}, $strip>>;

export type BucketUpdate = {
	frame: BucketFrame;
	kind: "frame";
} | {
	kind: "ended";
};

export class Consumer {
	constructor(broadcast: Broadcast.Consumer);
	bucket(bucket: string): BucketReader;
	prefix(path: string): Reader<PrefixFrame>;
	presence(label: string): Reader<PresenceFrame>;
	totals(): Reader<TotalsFrame>;
}

export type Content = infer<typeof ContentSchema>;

export const ContentSchema: ZodMiniPrefault<ZodMiniObject<{
	bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	frames: ZodMiniDefault<ZodMiniNumber<number>>;
	groups: ZodMiniDefault<ZodMiniNumber<number>>;
}, $strip>>;

export type Failures = infer<typeof FailuresSchema>;

export const FailuresSchema: ZodMiniPrefault<ZodMiniObject<{
	application: ZodMiniDefault<ZodMiniNumber<number>>;
	expired: ZodMiniDefault<ZodMiniNumber<number>>;
	idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
	transport: ZodMiniDefault<ZodMiniNumber<number>>;
}, $strip>>;

export interface NodeEvent {
	active: boolean;
	node: Path.Valid;
	path: Path.Valid;
}

export class Nodes {
	constructor(origin: Origin.Reader, prefix?: Path.Valid);
	close(): void;
	next(): Promise<NodeEvent | undefined>;
}

export const PREFIX: ".stats";

export type Prefix = infer<typeof PrefixSchema>;

export type PrefixFrame = infer<typeof PrefixFrameSchema>;

export const PrefixFrameSchema: ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
	children: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
		publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
	}, $strip>>>>;
	publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	self: ZodMiniPrefault<ZodMiniObject<{
		publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
	}, $strip>>;
	subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
}, $strip>>>;

export const PrefixSchema: ZodMiniPrefault<ZodMiniObject<{
	children: ZodMiniDefault<ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
		publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
	}, $strip>>>>;
	publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	self: ZodMiniPrefault<ZodMiniObject<{
		publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
		subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
			announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
			fetches: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
			inflated: ZodMiniDefault<ZodMiniNumber<number>>;
			stale: ZodMiniPrefault<ZodMiniObject<{
				bytes: ZodMiniDefault<ZodMiniNumber<number>>;
				frames: ZodMiniDefault<ZodMiniNumber<number>>;
				groups: ZodMiniDefault<ZodMiniNumber<number>>;
			}, $strip>>;
			subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
			subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>>;
	}, $strip>>;
	subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
}, $strip>>;

export type Presence = infer<typeof PresenceSchema>;

export type PresenceFrame = infer<typeof PresenceFrameSchema>;

export const PresenceFrameSchema: ZodMiniRecord<ZodMiniString<string>, ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
	announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
	received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	requests: ZodMiniDefault<ZodMiniNumber<number>>;
	routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
	sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
	sessions_failed: ZodMiniPrefault<ZodMiniObject<{
		application: ZodMiniDefault<ZodMiniNumber<number>>;
		expired: ZodMiniDefault<ZodMiniNumber<number>>;
		idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
		transport: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>;
	sessions_refused: ZodMiniPrefault<ZodMiniObject<{
		expired: ZodMiniDefault<ZodMiniNumber<number>>;
		forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
		membership: ZodMiniDefault<ZodMiniNumber<number>>;
		refused: ZodMiniDefault<ZodMiniNumber<number>>;
		request: ZodMiniDefault<ZodMiniNumber<number>>;
		unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>;
	sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
	wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
	wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
}, $strip>>>>;

export const PresenceSchema: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
	announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
	received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	requests: ZodMiniDefault<ZodMiniNumber<number>>;
	routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
	sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
	sessions_failed: ZodMiniPrefault<ZodMiniObject<{
		application: ZodMiniDefault<ZodMiniNumber<number>>;
		expired: ZodMiniDefault<ZodMiniNumber<number>>;
		idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
		transport: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>;
	sessions_refused: ZodMiniPrefault<ZodMiniObject<{
		expired: ZodMiniDefault<ZodMiniNumber<number>>;
		forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
		membership: ZodMiniDefault<ZodMiniNumber<number>>;
		refused: ZodMiniDefault<ZodMiniNumber<number>>;
		request: ZodMiniDefault<ZodMiniNumber<number>>;
		unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>;
	sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
	wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
	wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
}, $strip>>>;

export class Reader<T> {
	close(): void;
	next(): Promise<T | undefined>;
}

export type Refusals = infer<typeof RefusalsSchema>;

export const RefusalsSchema: ZodMiniPrefault<ZodMiniObject<{
	expired: ZodMiniDefault<ZodMiniNumber<number>>;
	forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
	membership: ZodMiniDefault<ZodMiniNumber<number>>;
	refused: ZodMiniDefault<ZodMiniNumber<number>>;
	request: ZodMiniDefault<ZodMiniNumber<number>>;
	unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
}, $strip>>;

export type Totals = infer<typeof TotalsSchema>;

export type TotalsFrame = infer<typeof TotalsFrameSchema>;

export const TotalsFrameSchema: ZodMiniRecord<ZodMiniString<string>, ZodMiniPrefault<ZodMiniObject<{
	publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	sessions: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		requests: ZodMiniDefault<ZodMiniNumber<number>>;
		routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
		sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_failed: ZodMiniPrefault<ZodMiniObject<{
			application: ZodMiniDefault<ZodMiniNumber<number>>;
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
			transport: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_refused: ZodMiniPrefault<ZodMiniObject<{
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
			membership: ZodMiniDefault<ZodMiniNumber<number>>;
			refused: ZodMiniDefault<ZodMiniNumber<number>>;
			request: ZodMiniDefault<ZodMiniNumber<number>>;
			unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
}, $strip>>>;

export const TotalsSchema: ZodMiniPrefault<ZodMiniObject<{
	publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	sessions: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announces_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		received_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		requests: ZodMiniDefault<ZodMiniNumber<number>>;
		routes_refused: ZodMiniDefault<ZodMiniNumber<number>>;
		sent_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		sessions_failed: ZodMiniPrefault<ZodMiniObject<{
			application: ZodMiniDefault<ZodMiniNumber<number>>;
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			idle_timeout: ZodMiniDefault<ZodMiniNumber<number>>;
			transport: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_refused: ZodMiniPrefault<ZodMiniObject<{
			expired: ZodMiniDefault<ZodMiniNumber<number>>;
			forbidden: ZodMiniDefault<ZodMiniNumber<number>>;
			membership: ZodMiniDefault<ZodMiniNumber<number>>;
			refused: ZodMiniDefault<ZodMiniNumber<number>>;
			request: ZodMiniDefault<ZodMiniNumber<number>>;
			unavailable: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		sessions_started: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_peak: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_rx: ZodMiniDefault<ZodMiniNumber<number>>;
		wire_tx: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
}, $strip>>;

export type Traffic = infer<typeof TrafficSchema>;

export const TrafficSchema: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
	announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
	announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
	broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
	broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
	bytes: ZodMiniDefault<ZodMiniNumber<number>>;
	datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
	fetches: ZodMiniDefault<ZodMiniNumber<number>>;
	frames: ZodMiniDefault<ZodMiniNumber<number>>;
	groups: ZodMiniDefault<ZodMiniNumber<number>>;
	inflated: ZodMiniDefault<ZodMiniNumber<number>>;
	stale: ZodMiniPrefault<ZodMiniObject<{
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>;
	subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
	subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
}, $strip>>>;

export type Usage = infer<typeof UsageSchema>;

export const UsageSchema: ZodMiniPrefault<ZodMiniObject<{
	publisher: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
	subscriber: ZodMiniPipe<ZodMiniTransform<unknown, unknown>, ZodMiniPrefault<ZodMiniObject<{
		announced_bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		announces_started: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		broadcasts_started: ZodMiniDefault<ZodMiniNumber<number>>;
		bytes: ZodMiniDefault<ZodMiniNumber<number>>;
		datagrams: ZodMiniDefault<ZodMiniNumber<number>>;
		fetches: ZodMiniDefault<ZodMiniNumber<number>>;
		frames: ZodMiniDefault<ZodMiniNumber<number>>;
		groups: ZodMiniDefault<ZodMiniNumber<number>>;
		inflated: ZodMiniDefault<ZodMiniNumber<number>>;
		stale: ZodMiniPrefault<ZodMiniObject<{
			bytes: ZodMiniDefault<ZodMiniNumber<number>>;
			frames: ZodMiniDefault<ZodMiniNumber<number>>;
			groups: ZodMiniDefault<ZodMiniNumber<number>>;
		}, $strip>>;
		subscriptions_ended: ZodMiniDefault<ZodMiniNumber<number>>;
		subscriptions_started: ZodMiniDefault<ZodMiniNumber<number>>;
	}, $strip>>>;
}, $strip>>;

export function bucketTrack(bucket: string, epoch: string): string;

export function parseBucketTrack(name: string): {
	bucket: string;
	epoch: string;
} | undefined;

export function parseNodePath(prefix: Path.Valid, path: Path.Valid): Path.Valid | undefined;

export function prefixTrack(path: string): string;

export function presenceTrack(label: string): string;

export function totalsTrack(): string;
```
