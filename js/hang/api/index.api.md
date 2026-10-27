# `@moq/hang`

```ts
import { $ZodBranded } from "zod/v4/core";
import { $brand } from "@zod/mini";
import { $loose } from "zod/v4/core";
import { $strip } from "zod/v4/core";
import { Micro } from "@moq/clock";
import { Window } from "@moq/json";
import { ZodMiniNullable } from "@zod/mini";
import { ZodMiniNumber } from "@zod/mini";
import { ZodMiniObject } from "@zod/mini";
import { ZodMiniOptional } from "@zod/mini";
import { ZodMiniPipe } from "@zod/mini";
import { ZodMiniRecord } from "@zod/mini";
import { ZodMiniString } from "@zod/mini";
import { ZodMiniTransform } from "@zod/mini";
import { infer } from "@zod/mini";

export * as Catalog from "@moq/hang/catalog";

export * as Container from "@moq/hang/container";

export namespace Echo {
	export type Arrival = infer<typeof Echo.ArrivalSchema>;
	export const ArrivalSchema: ZodMiniObject<{
		timestamp: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
	}, $strip>;
	export type Rendition = infer<typeof Echo.RenditionSchema>;
	export const RenditionSchema: ZodMiniObject<{
		arrival: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<ZodMiniObject<{
			timestamp: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
			wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		}, $strip>>, ZodMiniTransform<{
			timestamp: number & $brand<"u53">;
			wall: number & $brand<"u53">;
		} | undefined, {
			timestamp: number & $brand<"u53">;
			wall: number & $brand<"u53">;
		} | null>>>;
		bytes: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		decoded: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		errors: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		frames: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		late: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		latency: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		stalled: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		stalls: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		underruns: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	}, $strip>;
	export const SUFFIX: ".echo";
	export type Snapshot = infer<typeof Echo.SnapshotSchema>;
	export const SnapshotSchema: ZodMiniObject<{
		renditions: ZodMiniOptional<ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
			arrival: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<ZodMiniObject<{
				timestamp: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
				wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
			}, $strip>>, ZodMiniTransform<{
				timestamp: number & $brand<"u53">;
				wall: number & $brand<"u53">;
			} | undefined, {
				timestamp: number & $brand<"u53">;
				wall: number & $brand<"u53">;
			} | null>>>;
			bytes: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			decoded: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			errors: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			frames: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			late: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			latency: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			stalled: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			stalls: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			underruns: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		}, $strip>>>;
		transport: ZodMiniOptional<ZodMiniObject<{
			age: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			lostBytes: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			lostPackets: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			rate: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			rtt: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		}, $strip>>;
	}, $loose>;
	export const TRACK: "feedback.json";
	export function isEcho(path: Net.Path.Valid): boolean;
}

export namespace Health {
	export interface Connection {
		evidence: Health.Evidence[];
		lostBytes?: number;
		lostPackets?: number;
		stale?: true;
		verdict: Health.Verdict;
	}
	export interface Evidence {
		input: Health.Input;
		value?: number;
		verdict: Health.Verdict;
	}
	export interface Health {
		interval?: number;
		observer: Health.Observer;
		renditions: Record<string, Health.Rendition>;
		reset?: Health.Reset;
		role: Health.Role;
		stale?: true;
		transport: Health.Connection;
		verdict: Health.Verdict;
	}
	export type Input = "rtt" | "loss" | "headroom" | "skipped" | "late" | "errors" | "stalled";
	export interface Limits {
		degraded: number;
		unhealthy: number;
	}
	export interface Observer {
		epoch?: Net.Epoch.Valid;
		path: Net.Path.Valid;
	}
	export interface Rendition {
		bitrate?: number;
		bytes?: number;
		evidence: Health.Evidence[];
		frames?: number;
		verdict: Health.Verdict;
	}
	export interface Report<S> {
		observer: Health.Observer;
		snapshot: S;
	}
	export type Reset = "decrease" | "replaced";
	export type Role = "publisher" | "viewer";
	export interface Sample<S> {
		age: number;
		current: Health.Report<S>;
		interval: number;
		previous?: Health.Report<S>;
	}
	export interface Thresholds {
		publisher: {
			skipped: Health.Limits;
		};
		stale: number;
		transport: {
			headroom: Health.Limits;
			loss: Health.Limits;
			rtt: Health.Limits;
		};
		viewer: {
			errors: Health.Limits;
			late: Health.Limits;
			stalled: Health.Limits;
		};
	}
	export type Verdict = "unknown" | "healthy" | "degraded" | "unhealthy";
	export function defaultThresholds(): Health.Thresholds;
	export function publisher(sample: Health.Sample<Stats.Snapshot>, thresholds?: Health.Thresholds): Health.Health;
	export function viewer(sample: Health.Sample<Echo.Snapshot>, thresholds?: Health.Thresholds): Health.Health;
}

export namespace Hls {
	export class Consumer implements AsyncIterable<Hls.Event> {
		[asyncIterator](): AsyncIterator<Hls.Event>;
		close(): void;
		next(): Promise<Hls.Event | undefined>;
		static subscribe(broadcast: Net.Broadcast.Consumer, hls: import("@moq/hang/catalog").Hls): Hls.Consumer;
	}
	export type Entry = Omit<Hls.Record, "pts" | "duration"> & {
		duration?: Micro;
		pts: Micro;
	};
	export type Event = {
		push: {
			entry: Hls.Entry;
			index: number;
		};
	} | {
		pop: Window.Span;
	} | {
		skip: Window.Span;
	};
	export interface Record {
		duration?: number;
		group: number;
		pts: number;
		sequence: number;
	}
	export const SUFFIX: ".hls";
}

export * as Net from "@moq/net";

export * as Signals from "@moq/signals";

export namespace Stats {
	export type Rendition = infer<typeof Stats.RenditionSchema>;
	export const RenditionSchema: ZodMiniObject<{
		bitrate: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		bytes: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		frames: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		keyframes: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		skipped: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	}, $strip>;
	export type Snapshot = infer<typeof Stats.SnapshotSchema>;
	export const SnapshotSchema: ZodMiniObject<{
		renditions: ZodMiniOptional<ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
			bitrate: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			bytes: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			frames: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			keyframes: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			skipped: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		}, $strip>>>;
		transport: ZodMiniOptional<ZodMiniObject<{
			age: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			lostBytes: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			lostPackets: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			rate: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
			rtt: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		}, $strip>>;
	}, $loose>;
	export type Transport = infer<typeof Stats.TransportSchema>;
	export const TransportSchema: ZodMiniObject<{
		age: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		lostBytes: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		lostPackets: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		rate: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
		rtt: ZodMiniOptional<ZodMiniPipe<ZodMiniNullable<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53"> | null>>>;
	}, $strip>;
}
```
