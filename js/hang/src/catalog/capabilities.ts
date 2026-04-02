import * as z from "zod/mini";

export const VideoCapabilitiesSchema = z.object({
	hardware: z.optional(z.array(z.string())),
	software: z.optional(z.array(z.string())),
	unsupported: z.optional(z.array(z.string())),
});

export const AudioCapabilitiesSchema = z.object({
	hardware: z.optional(z.array(z.string())),
	software: z.optional(z.array(z.string())),
	unsupported: z.optional(z.array(z.string())),
});

export const CapabilitiesSchema = z.object({
	video: z.optional(VideoCapabilitiesSchema),
	audio: z.optional(AudioCapabilitiesSchema),
});

export type Capabilities = z.infer<typeof CapabilitiesSchema>;
export type VideoCapabilities = z.infer<typeof VideoCapabilitiesSchema>;
export type AudioCapabilities = z.infer<typeof AudioCapabilitiesSchema>;
