import type { Frame } from "./types";

export interface ContainerFormat {
	/** Parse one MoQ frame (raw bytes) into decoded media frames. */
	decode(frame: Uint8Array): Frame[];
}
