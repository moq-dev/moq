import type { Time } from "@moq/net";
import type { Frame } from "./types";

/** A container format that decodes raw MoQ frames into media frames. */
export interface Format {
	/** Parse one MoQ frame (raw bytes) into decoded media frames. */
	decode(frame: Uint8Array): Frame[];
	/** Return the endpoint timestamp carried by empty-payload metadata. */
	end?(frame: Frame): Time.Micro | undefined;
}
