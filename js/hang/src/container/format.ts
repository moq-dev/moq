import type { Time } from "@moq/net";
import type { Frame } from "./types";

/** A container format that decodes raw MoQ frames into media frames. */
export interface Format {
	/** Parse one MoQ frame's payload, given its moq-net timestamp (absent on an untimed track), into decoded media frames. */
	decode(payload: Uint8Array, timestamp: Time.Timestamp | undefined): Frame[];
	/** Return the endpoint timestamp carried by empty-payload metadata. */
	end?(frame: Frame): Time.Micro | undefined;
}
