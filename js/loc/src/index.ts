/**
 * Low Overhead Container (LOC): decode codec bitstreams framed with
 * per-frame timestamp and timescale metadata for MoQ.
 *
 * Two framings exist, chosen by the catalog, never by inspecting bytes:
 * {@link ObjectFormat} is draft-ietf-moq-loc-04's own, which an MSF `packaging: "loc"` track
 * uses, and {@link Format} is the hang catalog's `loc` container, which works on moq-lite too.
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
	/** The codec configuration (WebCodecs `description`) the frame carried, which replaces the catalog's from here on. */
	config?: Uint8Array;
}

const PROP_TIMESCALE = 0x08;
const PROP_VIDEO_FRAME_MARKING = 0x09;
const PROP_VIDEO_CONFIG = 0x0d;
const PROP_AUDIO_CONFIG = 0x0f;
const PROP_TIMESTAMP = 0x10;

// RFC 9626's I bit: the frame is independent, in both its short and long forms.
const FRAME_MARKING_INDEPENDENT = 0x20;

// The Timestamp id from draft-ietf-moq-loc-03, accepted on decode only. Draft-03's
// body text and its IANA table disagreed (0x0A vs 0x06); this is the table's value,
// which is what shipped. Draft-04 assigns 0x0A to Secure Objects private properties,
// so it is not accepted here.
const PROP_TIMESTAMP_DRAFT03 = 0x06;

const DEFAULT_TIMESCALE = 1_000_000;

/**
 * Decoder for the hang catalog's `loc` container, which carries LOC's properties inside the payload.
 *
 * Each MoQ frame is a small property block (timestamp, optional per-frame
 * timescale) followed by the codec bitstream payload. Frames without a 0x08
 * timescale property are interpreted as microseconds. moq-lite has no Object Properties, so this
 * in-payload block is how moq-dev carries LOC on every transport; it is not draft-ietf-moq-loc-04's
 * framing, which {@link ObjectFormat} decodes.
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

/**
 * Decoder for draft-ietf-moq-loc-04's framing: the public properties are the MoQ Object Properties
 * and the payload is the bare codec bitstream (section 2.2).
 *
 * The timestamp is 0x10, in units of the object's 0x08 Timescale, else the track's TIMESCALE, else
 * microseconds (section 2.3.1.2). Honouring an object-scope Timescale is LOC's rule for its own
 * Timestamp, not the transport's: moq-net never reads one, and times the frame by its track.
 * Video and Audio Config (0x0D, 0x0F) replace the catalog's codec description, and Video Frame
 * Marking (0x09) flags keyframes. Without it a frame is a keyframe only where its group starts.
 *
 * Only moq-transport carries Object Properties, so an object without them is refused rather than
 * guessed at: that track was not delivered with its framing.
 */
export class ObjectFormat {
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

	/** Decode one MoQ object, given its track-timed `timestamp` and its Object Properties. Throws when the properties or their Timestamp are missing or malformed. */
	decode(payload: Uint8Array, timestamp?: Moq.Time.Timestamp, properties?: Moq.Group.ObjectProperties): Frame[] {
		if (this.kind === "data" && payload.byteLength === 0) return [];
		if (properties === undefined) {
			throw new Error("loc: object has no Object Properties; LOC-04 framing needs moq-transport");
		}

		const time = properties.get(BigInt(PROP_TIMESTAMP));
		if (typeof time !== "bigint") throw new Error("loc: object missing the Timestamp property");

		const scale = properties.get(BigInt(PROP_TIMESCALE));
		let micros: bigint;
		if (scale !== undefined) {
			if (typeof scale !== "bigint" || scale === 0n) throw new Error("loc: Timescale property must be non-zero");
			micros = (time * BigInt(DEFAULT_TIMESCALE) + scale / 2n) / scale;
		} else if (timestamp !== undefined) {
			// The track's TIMESCALE already scaled this same Timestamp.
			micros = BigInt(Math.round(timestamp.as(Moq.Time.Timescale.MICRO)));
		} else {
			micros = time;
		}
		if (micros > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("loc: Timestamp exceeds 2^53 - 1 microseconds");

		const frame: Frame = { payload, timestamp: Number(micros) as Micro, keyframe: false };

		const marking = properties.get(BigInt(PROP_VIDEO_FRAME_MARKING));
		if (marking !== undefined) {
			if (!(marking instanceof Uint8Array) || marking.byteLength === 0) {
				throw new Error("loc: Video Frame Marking must be 1 to 4 bytes");
			}
			frame.keyframe = (marking[0] & FRAME_MARKING_INDEPENDENT) !== 0;
		}

		const config = properties.get(BigInt(this.kind === "audio" ? PROP_AUDIO_CONFIG : PROP_VIDEO_CONFIG));
		if (config !== undefined) {
			if (!(config instanceof Uint8Array)) throw new Error("loc: codec config property must be bytes");
			frame.config = config;
		}

		return [frame];
	}
}
