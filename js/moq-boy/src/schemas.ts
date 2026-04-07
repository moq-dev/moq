import { z } from "zod/mini";

/** Server-reported encoding/emulation performance stats. */
export const GameStatsSchema = z.object({
	video_secs: z.number(),
	audio_secs: z.number(),
	emulation_secs: z.number(),
	wall_secs: z.number(),
});

/** Per-frame status published by the emulator on the "status" track. */
export const GameStatusSchema = z.object({
	buttons: z.array(z.string()),
	latency: z.record(z.string(), z.number()),
	location: z.optional(z.string()),
	stats: z.optional(GameStatsSchema),
});

export type GameStats = z.infer<typeof GameStatsSchema>;
export type GameStatus = z.infer<typeof GameStatusSchema>;
