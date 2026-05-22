import type { Time } from "@moq/net";
import type { Format as ContainerFormat } from "../format";
import type { Frame } from "../types";
import { decodeDataSegment, type InitSegment } from "./decode";

export class Format implements ContainerFormat {
	#init: InitSegment;

	constructor(init: InitSegment) {
		this.#init = init;
	}

	decode(frame: Uint8Array): Frame[] {
		return decodeDataSegment(frame, this.#init).map((s) => ({
			data: s.data,
			timestamp: s.timestamp as Time.Micro,
			keyframe: s.keyframe,
		}));
	}
}
