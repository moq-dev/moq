import type { Time } from "@moq/lite";
import type { Format as ContainerFormat } from "../format";
import type { Frame } from "../types";
import { decodeDataSegment } from "./decode";

export class Format implements ContainerFormat {
	#timescale: number;

	constructor(timescale: number) {
		this.#timescale = timescale;
	}

	decode(frame: Uint8Array): Frame[] {
		return decodeDataSegment(frame, this.#timescale).map((s) => ({
			data: s.data,
			timestamp: s.timestamp as Time.Micro,
			keyframe: s.keyframe,
		}));
	}
}
