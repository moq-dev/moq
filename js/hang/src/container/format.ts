import type { Frame } from "./types";

export interface Format {
	/** Parse one MoQ frame (raw bytes) into decoded media frames. */
	decode(frame: Uint8Array): Frame[];
}
