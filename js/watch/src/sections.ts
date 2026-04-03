// Application-specific catalog section definitions.
// These define the JSON keys and Zod schemas for sections
// that are not part of the core media catalog (video/audio).

import { Section } from "@moq/hang/catalog";
import { z } from "zod";

const TrackSchema = z.object({
	name: z.string(),
});

export const ChatSchema = z.object({
	message: TrackSchema.optional(),
	typing: TrackSchema.optional(),
});
export type Chat = z.infer<typeof ChatSchema>;
export const CHAT = new Section("chat", ChatSchema);

export const UserSchema = z.object({
	id: z.string().optional(),
	name: z.string().optional(),
	avatar: z.string().optional(),
	color: z.string().optional(),
});
export type User = z.infer<typeof UserSchema>;
export const USER = new Section("user", UserSchema);

export const PreviewTrackSchema = z.object({
	name: z.string(),
});
export type PreviewTrack = z.infer<typeof PreviewTrackSchema>;
export const PREVIEW = new Section("preview", PreviewTrackSchema);

export const PreviewSchema = z.object({
	name: z.string().optional(),
	avatar: z.string().optional(),
	audio: z.boolean().optional(),
	video: z.boolean().optional(),
	typing: z.boolean().optional(),
	chat: z.boolean().optional(),
	screen: z.boolean().optional(),
});
export type Preview = z.infer<typeof PreviewSchema>;

export const PositionSchema = z.object({
	x: z.number().optional(),
	y: z.number().optional(),
	z: z.number().optional(),
	s: z.number().optional(),
});
export type Position = z.infer<typeof PositionSchema>;

export const LocationSchema = z.object({
	initial: PositionSchema.optional(),
	track: TrackSchema.optional(),
	handle: z.string().optional(),
	peers: TrackSchema.optional(),
});
export type Location = z.infer<typeof LocationSchema>;
export const LOCATION = new Section("location", LocationSchema);

export const PeersSchema = z.record(z.string(), PositionSchema);
export type Peers = z.infer<typeof PeersSchema>;
