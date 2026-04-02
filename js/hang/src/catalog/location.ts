import * as z from "zod/mini";
import { TrackSchema } from "./track";

export const PositionSchema = z.object({
	// The relative X position of the broadcast, from -1 to +1.
	// This should be used for audio panning but can also be used for video positioning.
	x: z.optional(z.number()),

	// The relative Y position of the broadcast, from -1 to +1.
	// This can be used for video positioning, and maybe audio panning.
	y: z.optional(z.number()),

	// The relative Z index of the broadcast, where larger values are closer to the viewer.
	// This is used to break ties when there are multiple broadcasts at the same position.
	z: z.optional(z.number()),

	// The scale of the broadcast, where 1 is 100%
	s: z.optional(z.number()),
});

export const LocationSchema = z.object({
	// The initial position of the broadcaster, from -1 to +1 in both dimensions.
	// If not provided, then the broadcaster is assumed to be at (0,0)
	// This should be used for audio panning but can also be used for video positioning.
	initial: z.optional(PositionSchema),

	// If provided, then updates to the position are done via a separate Moq track.
	// This is used to avoid a full catalog update every time we want to update a few bytes.
	// TODO: These updates currently use JSON for simplicity, but we should use a binary format.
	track: z.optional(TrackSchema),

	// If set, then this broadcaster allows other peers to request position updates via this handle.
	// We will have to discover and subscribe to their position updates.
	handle: z.optional(z.string()),

	// If provided, this broadcaster is signaling the location of other peers.
	// The payload is a JSON blob keyed by handle for each peer.
	peers: z.optional(TrackSchema),
});

export type Location = z.infer<typeof LocationSchema>;
export type Position = z.infer<typeof PositionSchema>;

export const PeersSchema = z.record(z.string(), PositionSchema);
export type Peers = z.infer<typeof PeersSchema>;
