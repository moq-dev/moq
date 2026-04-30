import type { Time } from "@moq/lite";

export interface DecodedFrame {
	data: Uint8Array;
	timestamp: Time.Micro;
	keyframe: boolean;
}

export interface ContainerFormat {
	/** Parse one MoQ frame (raw bytes) into decoded media frames. */
	decode(frame: Uint8Array): DecodedFrame[];
}
