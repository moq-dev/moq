import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { Format as ContainerFormat } from "./format";
import type { Frame } from "./types";

export class Format implements ContainerFormat {
	decode(frame: Uint8Array): Frame[] {
		const [timestamp, data] = Moq.Varint.decode(frame);
		return [{ data, timestamp: timestamp as Time.Micro, keyframe: false }];
	}
}

export interface Source {
	byteLength: number;
	copyTo(buffer: Uint8Array): void;
}

// A Helper class to encode frames into a track.
export class Producer {
	#track: Moq.Track;
	#group?: Moq.Group;

	constructor(track: Moq.Track) {
		this.#track = track;
	}

	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame(Producer.#encode(data, timestamp));
	}

	static #encode(source: Uint8Array | Source, timestamp: Time.Micro): Uint8Array {
		const timestampBytes = Moq.Varint.encode(timestamp);

		// Allocate buffer for timestamp + payload
		const payloadSize = source instanceof Uint8Array ? source.byteLength : source.byteLength;
		const data = new Uint8Array(timestampBytes.byteLength + payloadSize);

		// Write timestamp header
		data.set(timestampBytes, 0);

		// Write payload
		if (source instanceof Uint8Array) {
			data.set(source, timestampBytes.byteLength);
		} else {
			source.copyTo(data.subarray(timestampBytes.byteLength));
		}

		return data;
	}

	close(err?: Error) {
		this.#track.close(err);
		this.#group?.close();
	}
}
