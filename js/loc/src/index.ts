/**
 * Low Overhead Container (LOC): decode codec bitstreams framed with
 * per-frame timestamp and timescale metadata for MoQ.
 *
 * @module
 */

import type { Micro } from "@moq/clock";
import * as Moq from "@moq/net";

/** A decoded LOC frame: the codec bitstream plus its timing metadata. */
export interface Frame {
	/** The codec bitstream payload, with the LOC property block stripped. */
	payload: Uint8Array;
	/** Presentation timestamp in microseconds. */
	timestamp: Micro;
	/** True if this frame can be decoded without any preceding frames. */
	keyframe: boolean;
}

const PROP_TIMESCALE = 0x08;
const PROP_TIMESTAMP = 0x10;

// The Timestamp id from draft-ietf-moq-loc-03, accepted on decode only. Draft-03's
// body text and its IANA table disagreed (0x0A vs 0x06); this is the table's value,
// which is what shipped. Draft-04 assigns 0x0A to Secure Objects private properties,
// so it is not accepted here.
const PROP_TIMESTAMP_DRAFT03 = 0x06;

const DEFAULT_TIMESCALE = 1_000_000;

/**
 * Decoder for the Low Overhead Container (LOC) defined in
 * draft-ietf-moq-loc-04.
 *
 * Each MoQ frame is a small property block (timestamp, optional per-frame
 * timescale) followed by the codec bitstream payload. Frames without a 0x08
 * timescale property are interpreted as microseconds.
 */
export class Format {
	/** The kind of content carried by the track. */
	readonly kind: "audio" | "video" | "data";

	/** Configure endpoint metadata for audio or video; opaque data is the default. */
	constructor(kind: "audio" | "video" | "data" = "data") {
		this.kind = kind;
	}

	/** Return the video-frame or audio-source endpoint for an empty codec payload. */
	end(frame: Frame): Micro | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0 ? frame.timestamp : undefined;
	}

	/** Decode one moq-net frame into its LOC frames. On a data track an object with no bytes is the discontinuity marker, so it decodes to no frame. Throws on malformed input. */
	decode(frame: Uint8Array): Frame[] {
		if (this.kind === "data" && frame.byteLength === 0) return [];
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

			if (abs === PROP_TIMESTAMP || abs === PROP_TIMESTAMP_DRAFT03) {
				[timestamp, cursor] = Moq.Varint.decode(cursor);
			} else if (abs === PROP_TIMESCALE) {
				let value: number;
				[value, cursor] = Moq.Varint.decode(cursor);
				if (value === 0) {
					throw new Error("loc: timescale property must be non-zero");
				}
				timescale = value;
			} else if (abs % 2 === 0) {
				// An unknown varint property may use all 62 bits, which a number can't hold; skip it.
				cursor = Moq.Varint.decodeBigInt(cursor)[1];
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
		const micros = Math.round((timestamp * DEFAULT_TIMESCALE) / activeTimescale) as Micro;

		return [{ payload, timestamp: micros, keyframe: false }];
	}
}
