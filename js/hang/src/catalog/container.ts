import * as z from "zod/mini";
import { u53Schema } from "./integers";

/**
 * Container format for frame timestamp encoding and frame payload structure.
 *
 * - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads.
 *             Timestamps are in microseconds.
 * - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
 *           The init segment (ftyp+moov) is base64-encoded in the catalog.
 */
export const ContainerSchema = z._default(
	z.discriminatedUnion("kind", [
		// The default hang container
		z.object({ kind: z.literal("legacy") }),
		// CMAF container with base64-encoded init segment (ftyp+moov).
		// `timescale` and `trackId` are deprecated: they duplicate info in `init`
		// and are accepted only so catalogs from newer publishers (which still
		// emit them for older players) round-trip cleanly.
		z.object({
			kind: z.literal("cmaf"),
			init: z.base64(),
			timescale: z.optional(u53Schema),
			trackId: z.optional(u53Schema),
		}),
	]),
	{ kind: "legacy" },
);

export type Container = z.infer<typeof ContainerSchema>;
