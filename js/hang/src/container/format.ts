import type { Duration } from "@moq/clock";
import type { Time } from "@moq/net";
import type { Frame } from "./types";

/** A container format that decodes raw MoQ frames into media frames. */
export interface Format {
	/** The track's media role, when the format knows it; a data track decodes an object with no bytes as its discontinuity marker. */
	readonly kind?: "audio" | "video" | "data";
	/** Parse one MoQ frame's payload, given its moq-net timestamp (unscaled, its group's key, on a scale-less track), into decoded media frames. On a data track no frames means the discontinuity marker, so a format returns none only for an object with no bytes. */
	decode(payload: Uint8Array, timestamp: Time.Timestamp): Frame[];
	/** Return the endpoint timestamp carried by empty-payload metadata. */
	end?(frame: Frame): Duration | undefined;
}
