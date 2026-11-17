import { z } from "zod/mini";

/** Server-reported encoding/emulation performance stats. */
export const GameStatsSchema = z.object({
	video_secs: z.number(),
	audio_secs: z.number(),
	emulation_secs: z.number(),
	wall_secs: z.number(),
});

/** One component of a viewer's latency. */
export interface LatencyEntry {
	/** The component's name. */
	label: string;
	/** Its duration in milliseconds. */
	ms: number;
}

const LatencyEntrySchema: z.ZodMiniType<LatencyEntry> = z.object({
	label: z.string(),
	ms: z.number().check(z.int(), z.nonnegative(), z.lte(4294967295)),
});

/** Per-frame status published by the emulator on the "status" track. */
export const GameStatusSchema = z.object({
	buttons: z.array(z.string()),
	/** Each viewer's latency breakdown, keyed by viewer id. The player count is the number of keys. */
	latency: z.record(z.string(), z.array(LatencyEntrySchema)),
	location: z.optional(z.string()),
	stats: z.optional(GameStatsSchema),
});

/** Server-reported encoding and emulation performance stats. */
export type GameStats = z.infer<typeof GameStatsSchema>;
/** Per-frame status published by the emulator. */
export type GameStatus = z.infer<typeof GameStatusSchema>;
