import type { Duration } from "@moq/clock";
import type { Time } from "@moq/net";
import type { Frame } from "./types.ts";

/** A container format that decodes raw MoQ frames into media frames. */
export interface Format {
	/** The track's media role, when the format knows it; a data track decodes an object with no bytes as an older publisher's discontinuity marker. */
	readonly kind?: "audio" | "video" | "data";
	/** Parse one MoQ frame's payload, given its moq-net timestamp (unscaled, its group's key, on a scale-less track), into decoded media frames. On a data track no frames means an older publisher's discontinuity marker, so a format returns none only for an object with no bytes. */
	decode(payload: Uint8Array, timestamp: Time.Timestamp): Frame[];
	/** Return the endpoint an empty payload carries: an audio group's terminal marker, or an older publisher's duration or discontinuity marker. */
	end?(frame: Frame): Duration | undefined;
}
