# `@moq/boy`

```ts
import { $strip } from "zod/v4/core";
import { ZodMiniArray } from "@zod/mini";
import { ZodMiniNumber } from "@zod/mini";
import { ZodMiniObject } from "@zod/mini";
import { ZodMiniOptional } from "@zod/mini";
import { ZodMiniRecord } from "@zod/mini";
import { ZodMiniString } from "@zod/mini";
import { infer } from "@zod/mini";

export class Game {
	constructor(config: GameConfig);
	readonly active: Moq.Signals.Signal<boolean>;
	readonly audioDecoder: Watch.Audio.Decoder;
	readonly audioEmitter: Watch.Audio.Emitter;
	readonly audioSource: Watch.Audio.Source;
	readonly broadcast: Watch.Broadcast;
	readonly canvas: Moq.Signals.Signal<HTMLCanvasElement | undefined>;
	close(): void;
	readonly delay: Moq.Signals.Signal<Watch.Delay>;
	readonly expanded: Moq.Signals.Signal<string | undefined>;
	readonly heldButtons: Set<string>;
	readonly hovered: Moq.Signals.Signal<boolean>;
	sendButtons(): void;
	sendCommand(cmd: Record<string, unknown>): void;
	readonly sessionId: string;
	readonly status: Moq.Signals.Signal<{
		buttons: string[];
		latency: Record<string, {
			label: string;
			ms: number;
		}[]>;
		location?: string;
		stats?: {
			audio_secs: number;
			emulation_secs: number;
			video_secs: number;
			wall_secs: number;
		};
	} | undefined>;
	readonly sync: Watch.Sync;
	readonly userMuted: Moq.Signals.Signal<boolean>;
	readonly videoDecoder: Watch.Video.Decoder;
	readonly videoRenderer: Watch.Video.Renderer;
	readonly videoSource: Watch.Video.Source;
	readonly viewerId: Moq.Signals.Signal<string | undefined>;
	readonly volume: Moq.Signals.Signal<number>;
}

export interface GameConfig {
	connection: Moq.Connection;
	expanded: Moq.Signals.Signal<string | undefined>;
	gamePrefix: string;
	origin: Moq.Origin.Producer;
	sessionId: string;
	viewerPrefix: string;
}

export type GameStats = infer<typeof GameStatsSchema>;

export const GameStatsSchema: ZodMiniObject<{
	audio_secs: ZodMiniNumber<number>;
	emulation_secs: ZodMiniNumber<number>;
	video_secs: ZodMiniNumber<number>;
	wall_secs: ZodMiniNumber<number>;
}, $strip>;

export type GameStatus = infer<typeof GameStatusSchema>;

export const GameStatusSchema: ZodMiniObject<{
	buttons: ZodMiniArray<ZodMiniString<string>>;
	latency: ZodMiniRecord<ZodMiniString<string>, ZodMiniArray<ZodMiniObject<{
		label: ZodMiniString<string>;
		ms: ZodMiniNumber<number>;
	}, $strip>>>;
	location: ZodMiniOptional<ZodMiniString<string>>;
	stats: ZodMiniOptional<ZodMiniObject<{
		audio_secs: ZodMiniNumber<number>;
		emulation_secs: ZodMiniNumber<number>;
		video_secs: ZodMiniNumber<number>;
		wall_secs: ZodMiniNumber<number>;
	}, $strip>>;
}, $strip>;

export const KEY_MAP: Record<string, string>;

export * as Moq from "@moq/net";

export { default as MoqBoy } from "@moq/boy/element";

export * as Watch from "@moq/watch";
```
