import type { Time } from "@moq/net";
import type { Format as ContainerFormat } from "../format";
import type { Frame } from "../types";
import { decodeDataSegment, type InitSegment } from "./decode";

/** CMAF container format: decodes each MoQ frame as a moof+mdat fragment using the parsed init segment. */
export class Format implements ContainerFormat {
	#init: InitSegment;

	/** Create a format bound to the given parsed init segment (timescale, codec defaults). */
	constructor(init: InitSegment) {
		this.#init = init;
	}

	/** Decode one CMAF fragment into its media frames. */
	decode(frame: Uint8Array): Frame[] {
		return decodeDataSegment(frame, this.#init).map((s) => ({
			data: s.data,
			timestamp: s.timestamp as Time.Micro,
			keyframe: s.keyframe,
		}));
	}
}
