import type { Time } from "@moq/lite";
import type { ContainerFormat, DecodedFrame } from "../format";
import { decodeDataSegment } from "./decode";

export class CmafFormat implements ContainerFormat {
	#timescale: number;

	constructor(timescale: number) {
		this.#timescale = timescale;
	}

	decode(frame: Uint8Array): DecodedFrame[] {
		return decodeDataSegment(frame, this.#timescale).map((s) => ({
			data: s.data,
			timestamp: s.timestamp as Time.Micro,
			keyframe: s.keyframe,
		}));
	}
}
