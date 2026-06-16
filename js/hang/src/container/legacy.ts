import type { Time } from "@moq/net";
import { Varint } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { Format as ContainerFormat } from "./format";
import type { Frame } from "./types";

export class Format implements ContainerFormat {
	decode(frame: Uint8Array): Frame[] {
		const [timestamp, data] = Varint.decode(frame);
		return [{ data, timestamp: timestamp as Time.Micro, keyframe: false }];
	}
}

export interface Source {
	byteLength: number;
	copyTo(buffer: Uint8Array): void;
}

// Encode a frame as a timestamp varint followed by the payload bytes.
export function encodeFrame(source: Uint8Array | Source, timestamp: Time.Micro): Uint8Array {
	const timestampBytes = Varint.encode(timestamp);
	const data = new Uint8Array(timestampBytes.byteLength + source.byteLength);
	data.set(timestampBytes, 0);

	if (source instanceof Uint8Array) {
		data.set(source, timestampBytes.byteLength);
	} else {
		source.copyTo(data.subarray(timestampBytes.byteLength));
	}

	return data;
}
