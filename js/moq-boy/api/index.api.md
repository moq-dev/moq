# `@moq/boy`

```ts
import { $strip } from "zod/v4/core";
import { ZodMiniArray } from "zod/mini";
import { ZodMiniNumber } from "zod/mini";
import { ZodMiniObject } from "zod/mini";
import { ZodMiniOptional } from "zod/mini";
import { ZodMiniString } from "zod/mini";
import { infer } from "zod/mini";

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
	location: ZodMiniOptional<ZodMiniString<string>>;
	players: ZodMiniNumber<number>;
	stats: ZodMiniOptional<ZodMiniObject<{
		audio_secs: ZodMiniNumber<number>;
		emulation_secs: ZodMiniNumber<number>;
		video_secs: ZodMiniNumber<number>;
		wall_secs: ZodMiniNumber<number>;
	}, $strip>>;
}, $strip>;

export interface LatencyEntry {
	label: string;
	ms: number;
}

export { default as MoqBoy } from "@moq/boy/element";
```
