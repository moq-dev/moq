import type { Time } from "@moq/net";
import * as Moq from "@moq/net";

export interface Frame {
	data: Uint8Array;
	timestamp: Time.Micro;
	keyframe: boolean;
}

const PROP_TIMESTAMP = 0x06;
const PROP_TIMESCALE = 0x08;

const DEFAULT_TIMESCALE = 1_000_000;

/**
 * Decoder for the Low Overhead Container (LOC) defined in
 * draft-ietf-moq-loc.
 *
 * Each MoQ frame is a small property block (timestamp, optional per-frame
 * timescale) followed by the codec bitstream payload. Frames without a 0x08
 * timescale property are interpreted as microseconds.
 */
export class Format {
	decode(frame: Uint8Array): Frame[] {
		const [propsLen, afterLen] = Moq.Varint.decode(frame);
		if (afterLen.byteLength < propsLen) {
			throw new Error("loc: properties_length exceeds frame size");
		}
		const props = afterLen.subarray(0, propsLen);
		const payload = afterLen.subarray(propsLen);

		let timestamp: number | undefined;
		let timescale: number | undefined;
		let prevType = 0;
		let first = true;
		let cursor = props;

		while (cursor.byteLength > 0) {
			const [delta, afterDelta] = Moq.Varint.decode(cursor);
			const abs = first ? delta : prevType + delta;
			first = false;
			prevType = abs;
			cursor = afterDelta;

			if (abs % 2 === 0) {
				const [value, afterValue] = Moq.Varint.decode(cursor);
				cursor = afterValue;
				if (abs === PROP_TIMESTAMP) {
					timestamp = value;
				} else if (abs === PROP_TIMESCALE) {
					if (value === 0) {
						throw new Error("loc: timescale property must be non-zero");
					}
					timescale = value;
				}
			} else {
				const [len, afterLenInner] = Moq.Varint.decode(cursor);
				if (afterLenInner.byteLength < len) {
					throw new Error("loc: property length exceeds remaining bytes");
				}
				cursor = afterLenInner.subarray(len);
			}
		}

		if (timestamp === undefined) {
			throw new Error("loc: frame missing required timestamp property");
		}

		const activeTimescale = timescale ?? DEFAULT_TIMESCALE;
		const micros = Math.round((timestamp * DEFAULT_TIMESCALE) / activeTimescale) as Time.Micro;

		return [{ data: payload, timestamp: micros, keyframe: false }];
	}
}

export interface Source {
	byteLength: number;
	copyTo(buffer: Uint8Array): void;
}

/**
 * Encoder that packages frames as LOC and writes them to a moq-net track.
 *
 * Each call to {@link encode} produces one moq-net frame containing a
 * property block with the 0x06 timestamp (in microseconds) and the codec
 * bitstream payload.
 */
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

		this.#group?.writeFrame(this.#encode(data, timestamp));
	}

	#encode(source: Uint8Array | Source, timestamp: Time.Micro): Uint8Array {
		const propTypeBytes = Moq.Varint.encode(PROP_TIMESTAMP);
		const propValueBytes = Moq.Varint.encode(timestamp);
		const propsLen = propTypeBytes.byteLength + propValueBytes.byteLength;

		const propsLenBytes = Moq.Varint.encode(propsLen);

		const payloadSize = source.byteLength;
		const total = propsLenBytes.byteLength + propsLen + payloadSize;
		const out = new Uint8Array(total);

		let offset = 0;
		out.set(propsLenBytes, offset);
		offset += propsLenBytes.byteLength;
		out.set(propTypeBytes, offset);
		offset += propTypeBytes.byteLength;
		out.set(propValueBytes, offset);
		offset += propValueBytes.byteLength;

		const payloadView = out.subarray(offset);
		if (source instanceof Uint8Array) {
			payloadView.set(source);
		} else {
			source.copyTo(payloadView);
		}

		return out;
	}

	close(err?: Error) {
		this.#group?.close();
		this.#track.close(err);
	}
}
