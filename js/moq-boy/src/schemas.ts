import { z } from "zod/mini";

/** Server-reported encoding/emulation performance stats. */
export const GameStatsSchema = z.object({
	video_secs: z.number(),
	audio_secs: z.number(),
	emulation_secs: z.number(),
	wall_secs: z.number(),
});

/** A single latency component measurement. */
export const LatencyEntrySchema = z.object({
	label: z.string(),
	ms: z.number().check(z.int(), z.nonnegative(), z.lte(4294967295)),
});

/** Per-frame status published by the emulator on the "status" track. */
export const GameStatusSchema = z.object({
	buttons: z.array(z.string()),
	players: z.number().check(z.int(), z.nonnegative()),
	location: z.optional(z.string()),
	stats: z.optional(GameStatsSchema),
});

/** What the emulator writes back on a viewer's session, one per command. */
export const ReplySchema = z.object({
	latency: z.array(LatencyEntrySchema),
});

/** Server-reported encoding and emulation performance stats. */
export type GameStats = z.infer<typeof GameStatsSchema>;
/** One component of a viewer's latency, in milliseconds. */
export type LatencyEntry = z.infer<typeof LatencyEntrySchema>;
/** Per-frame status published by the emulator. */
export type GameStatus = z.infer<typeof GameStatusSchema>;
