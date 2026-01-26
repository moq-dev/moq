import { z } from "zod";
import { u53Schema } from "./integers";

/**
 * Container format for frame timestamp encoding and frame payload structure.
 *
 * - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads.
 *             Timestamps are in microseconds.
 * - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
 *           Timestamps are in timescale units.
 */
export const ContainerSchema = z
	.discriminatedUnion("kind", [
		// The default hang container
		z.object({ kind: z.literal("legacy") }),
		// CMAF container with timescale for timestamp conversion
		z.object({
			kind: z.literal("cmaf"),
			// Time units per second
			timescale: u53Schema,
			// Track ID used in the moof/mdat fragments
			trackId: u53Schema,
		}),
	])
	.default({ kind: "legacy" });

export type Container = z.infer<typeof ContainerSchema>;
