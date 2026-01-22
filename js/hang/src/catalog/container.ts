import { z } from "zod";
import { TrackSchema } from "./track";

/**
 * Container format for frame timestamp encoding and frame payload structure.
 *
 * - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads
 * - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments and an init track
 */
export const ContainerSchema = z
	.discriminatedUnion("kind", [
		// The default hang container
		z.object({ kind: z.literal("legacy") }),
		// Contains the name of the init track
		z.object({ kind: z.literal("cmaf"), init_track: TrackSchema }),
	])
	.default({ kind: "legacy" });

export type Container = z.infer<typeof ContainerSchema>;

export const CONTAINER = {
	legacy: { kind: "legacy" },
	cmaf: { kind: "cmaf", init_track: TrackSchema },
} as const;
