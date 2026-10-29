import type { Micro } from "@moq/clock";
import { type Time, Varint } from "@moq/net";
import { decodeDataSegment, type InitSegment } from "../cmaf/decode";
import { isSync, present } from "../cmaf/sample";
import type { Format as ContainerFormat } from "../container/format";
import type { Frame } from "../container/types";

// Element types, draft-einarsson-moq-locmaf-01 Section 7.1.
const GEN_BOX = 1;
const FULL_HEADER = 2;
const DELTA_HEADER = 3;
const RAW_BOXES = 4;

// Header field IDs, Section 10. An even ID is a scalar, an odd ID is length-prefixed.
const SAMPLE_DESCRIPTION_INDEX = 2;
const DEFAULT_SAMPLE_DURATION = 4;
const DEFAULT_SAMPLE_SIZE = 6;
const DEFAULT_SAMPLE_FLAGS = 8;
const BASE_MEDIA_DECODE_TIME = 10;
const FIRST_SAMPLE_FLAGS = 12;
const SAMPLE_COUNT = 14;
const SAMPLE_SIZES = 1;
const SAMPLE_DURATIONS = 3;
const COMPOSITION_TIME_OFFSETS = 5;
const SAMPLE_FLAGS = 7;
const DELETED = 27;
// senc fields: per-sample IVs, subsample maps, and the IV size.
const CENC = new Set([9, 11, 13, 15, 16]);

const SCALARS = new Set([
	SAMPLE_DESCRIPTION_INDEX,
	DEFAULT_SAMPLE_DURATION,
	DEFAULT_SAMPLE_SIZE,
	DEFAULT_SAMPLE_FLAGS,
	BASE_MEDIA_DECODE_TIME,
	FIRST_SAMPLE_FLAGS,
	SAMPLE_COUNT,
]);
const LISTS = new Set([SAMPLE_SIZES, SAMPLE_DURATIONS, COMPOSITION_TIME_OFFSETS, SAMPLE_FLAGS]);

/** A header's represented fields: a scalar or a per-sample list per field ID. */
type Fields = Map<number, number | number[]>;

/** The in-group reference a delta header applies to: the previous chunk's fields and timeline. */
interface Reference {
	fields: Fields;
	/** The chunk's base media decode time, in media ticks. */
	bmdt: number;
	/** The sum of the chunk's sample durations, in media ticks. */
	duration: number;
}

/** Reads the vi64 values (draft-ietf-moq-transport Section 1.4.1) of an object front to back. */
class Cursor {
	#data: Uint8Array;
	#offset = 0;

	constructor(data: Uint8Array) {
		this.#data = data;
	}

	get done(): boolean {
		return this.#offset >= this.#data.byteLength;
	}

	/** The bytes not yet read. */
	rest(): Uint8Array {
		return this.#data.subarray(this.#offset);
	}

	vi64(): number {
		const rest = this.rest();
		// One byte holds 0 to 127, the common case, without allocating a BigInt.
		const first = rest.at(0);
		if (first !== undefined && first < 0x80) {
			this.#offset++;
			return first;
		}
		const [value, after] = Varint.decodeLeadingOnes(rest);
		if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error(`LOCMAF value ${value} exceeds 2^53`);
		this.#offset += rest.byteLength - after.byteLength;
		return Number(value);
	}

	bytes(length: number): Uint8Array {
		if (length > this.#data.byteLength - this.#offset) throw new Error("Truncated LOCMAF element");
		const bytes = this.#data.subarray(this.#offset, this.#offset + length);
		this.#offset += length;
		return bytes;
	}
}

/** Section 7.4: the signed value a zigzag vi64 carries. */
function zigzag(z: number): number {
	return z % 2 === 0 ? z / 2 : -(z + 1) / 2;
}

/** The vi64 elements of a list field, zigzag decoded when `signed`. */
function list(bytes: Uint8Array, signed: boolean): number[] {
	const cursor = new Cursor(bytes);
	const values: number[] = [];
	while (!cursor.done) {
		const value = cursor.vi64();
		values.push(signed ? zigzag(value) : value);
	}
	return values;
}

/** Section 7.3: a property block's (field_id, value) tuples, an even ID's vi64 or an odd ID's bytes. */
function properties(block: Uint8Array): Map<number, number | Uint8Array> {
	const cursor = new Cursor(block);
	const props = new Map<number, number | Uint8Array>();
	while (!cursor.done) {
		const id = cursor.vi64();
		if (props.has(id)) throw new Error(`LOCMAF header repeats field ${id}`);
		props.set(id, id % 2 === 0 ? cursor.vi64() : cursor.bytes(cursor.vi64()));
		if (CENC.has(id)) throw new Error("Encrypted (CENC) LOCMAF chunks are unsupported");
	}
	return props;
}

/** How many elements a list field holds for `count` samples: one less for sizes (Section 10.2). */
function lengthOf(id: number, count: number): number {
	return id === SAMPLE_SIZES ? Math.max(count - 1, 0) : count;
}

/** Section 11: the fields of a full header, absolute. */
function full(props: Map<number, number | Uint8Array>): Fields {
	if (props.has(DELETED)) throw new Error("LOCMAF full header carries deltaDeletedLocmafIDs");
	const fields: Fields = new Map();
	for (const [id, value] of props) {
		if (SCALARS.has(id)) fields.set(id, value as number);
		// Composition offsets are signed, so they are zigzag even in a full header.
		else if (LISTS.has(id)) fields.set(id, list(value as Uint8Array, id === COMPOSITION_TIME_OFFSETS));
		// Section 7.3: an unknown field is skipped by its parity and otherwise ignored.
	}
	if (!fields.has(SAMPLE_COUNT)) throw new Error("LOCMAF full header lacks trunSampleCount");
	if (!fields.has(BASE_MEDIA_DECODE_TIME)) throw new Error("LOCMAF full header lacks tfdtBaseMediaDecodeTime");
	return fields;
}

/** Section 12: the fields of a delta header, applied to the previous chunk's. */
function delta(props: Map<number, number | Uint8Array>, previous: Fields, max: number): Fields {
	if (props.has(BASE_MEDIA_DECODE_TIME)) throw new Error("LOCMAF delta header carries tfdtBaseMediaDecodeTime");
	const fields: Fields = new Map(previous);

	// Section 12.3: deletions apply first, so a deleted field's delta starts from zero.
	const deleted = props.get(DELETED);
	if (deleted !== undefined) {
		for (const id of list(deleted as Uint8Array, false)) fields.delete(id);
	}

	// Only composition offsets are signed, so a delta taking anything else below zero is malformed.
	const unsigned = (id: number, value: number) => {
		if (value < 0) throw new Error(`LOCMAF delta header makes field ${id} ${value}`);
		return value;
	};
	for (const [id, value] of props) {
		if (!SCALARS.has(id)) continue;
		fields.set(id, unsigned(id, ((fields.get(id) as number | undefined) ?? 0) + zigzag(value as number)));
	}
	const count = fields.get(SAMPLE_COUNT) as number | undefined;
	if (count === undefined) throw new Error("LOCMAF delta header deletes trunSampleCount");
	if (count > max) throw new Error(`LOCMAF chunk of ${count} samples has only ${max} mdat bytes`);

	// Section 12.1.1: each list takes the new count's length; an element past the previous list's
	// end adds to zero, and one past the new length is dropped. A list the header leaves out keeps
	// its elements, as a delta of zero would.
	for (const id of LISTS) {
		const before = fields.get(id) as number[] | undefined;
		const bytes = props.get(id) as Uint8Array | undefined;
		if (before === undefined && bytes === undefined) continue;
		const changes = bytes ? list(bytes, true) : [];
		const length = lengthOf(id, count);
		if (bytes && changes.length !== length) {
			throw new Error(`LOCMAF delta list ${id} holds ${changes.length} elements, not ${length}`);
		}
		fields.set(
			id,
			Array.from({ length }, (_, i) => {
				const value = (before?.[i] ?? 0) + (changes[i] ?? 0);
				return id === COMPOSITION_TIME_OFFSETS ? value : unsigned(id, value);
			}),
		);
	}
	return fields;
}

/**
 * LOCMAF container format (draft-einarsson-moq-locmaf-01, `locmafVersion` "0.3"): each MoQ frame is
 * one CMAF chunk whose `moof` travels as a compact full or delta header ahead of the raw `mdat`.
 *
 * Decodes straight to frames, the draft's frame interface, without rebuilding the `moof`. A delta
 * header refers to the previous chunk in its group, so a consumer decodes each group through its
 * own {@link Format.group}.
 */
export class Format implements ContainerFormat {
	#init: InitSegment;
	// The previous chunk of this group, unset before its first full header and after rawBoxes.
	#reference?: Reference;

	/** Create a format bound to the given parsed init segment (timescale, trex defaults). */
	constructor(init: InitSegment) {
		this.#init = init;
	}

	/** A decoder for one more group, sharing the init segment but not the delta chain. */
	group(): Format {
		return new Format(this.#init);
	}

	/** Decode one LOCMAF object, presenting its earliest sample at the moq-net `timestamp`, or at its BMDT if untimed. */
	decode(payload: Uint8Array, timestamp: Time.Timestamp | undefined): Frame[] {
		const cursor = new Cursor(payload);
		let type = cursor.vi64();

		if (type === RAW_BOXES) {
			// Section 9: verbatim boxes, a plain moof+mdat here, which also restart the delta chain.
			this.#reference = undefined;
			return decodeDataSegment(cursor.rest(), this.#init, timestamp).map((s) => ({
				payload: s.data,
				timestamp: s.timestamp as Micro,
				keyframe: s.keyframe,
				duration: s.duration as Micro,
			}));
		}

		// Section 8: boxes that render ahead of the moof (styp, prft, emsg) carry nothing to decode.
		while (type === GEN_BOX) {
			const size = cursor.vi64();
			if (size < 4 || size > 0xffff_fffb) throw new Error(`Invalid LOCMAF genBox size ${size}`);
			cursor.bytes(size);
			type = cursor.vi64();
		}
		if (type !== FULL_HEADER && type !== DELTA_HEADER) {
			throw new Error(`Invalid LOCMAF element type ${type} where a header belongs`);
		}

		const props = properties(cursor.bytes(cursor.vi64()));
		const mdat = cursor.rest();

		let fields: Fields;
		let bmdt: number;
		if (type === FULL_HEADER) {
			fields = full(props);
			bmdt = fields.get(BASE_MEDIA_DECODE_TIME) as number;
		} else {
			const reference = this.#reference;
			if (!reference) throw new Error("LOCMAF delta header without a full header before it in its group");
			fields = delta(props, reference.fields, mdat.byteLength);
			// Section 12.2: a delta chunk continues where the previous chunk's samples ended.
			bmdt = reference.bmdt + reference.duration;
		}

		// Every sample holds at least one byte, which bounds what a hostile count can allocate.
		const count = fields.get(SAMPLE_COUNT) as number;
		if (count > mdat.byteLength)
			throw new Error(`LOCMAF chunk of ${count} samples has only ${mdat.byteLength} mdat bytes`);
		for (const id of LISTS) {
			const values = fields.get(id) as number[] | undefined;
			if (values && values.length !== lengthOf(id, count)) {
				throw new Error(`LOCMAF list ${id} holds ${values.length} elements for ${count} samples`);
			}
		}

		const sizes = this.#sizes(fields, count, mdat.byteLength);
		const durations = fields.get(SAMPLE_DURATIONS) as number[] | undefined;
		const offsets = fields.get(COMPOSITION_TIME_OFFSETS) as number[] | undefined;
		const flags = fields.get(SAMPLE_FLAGS) as number[] | undefined;
		const defaultDuration =
			(fields.get(DEFAULT_SAMPLE_DURATION) as number | undefined) ?? this.#init.defaultSampleDuration;
		const defaultFlags = (fields.get(DEFAULT_SAMPLE_FLAGS) as number | undefined) ?? this.#init.defaultSampleFlags;
		const firstFlags = fields.get(FIRST_SAMPLE_FLAGS) as number | undefined;

		// Section 15.1: each sample's effective values.
		const frames: Frame[] = [];
		const ptss: number[] = [];
		let offset = 0;
		let dts = bmdt;
		for (let i = 0; i < count; i++) {
			const duration = durations?.[i] ?? defaultDuration;
			const sampleFlags = flags?.[i] ?? (i === 0 ? firstFlags : undefined) ?? defaultFlags;
			ptss.push(dts + (offsets?.[i] ?? 0));
			frames.push({
				payload: mdat.subarray(offset, offset + sizes[i]),
				timestamp: 0 as Micro, // Set below, once the earliest presentation time is known.
				// Audio never reports a keyframe, as in CMAF: the group start is the consumer's to mark.
				keyframe: this.#init.kind === "video" && isSync(sampleFlags),
				duration: Math.round((duration * 1_000_000) / this.#init.timescale) as Micro,
			});
			offset += sizes[i];
			dts += duration;
		}

		const timestamps = present(ptss, timestamp, this.#init.timescale);
		for (const [i, frame] of frames.entries()) frame.timestamp = timestamps[i] as Micro;

		this.#reference = { fields, bmdt, duration: dts - bmdt };
		return frames;
	}

	/** Section 11.2: each sample's size, from the header and the mdat payload's length. */
	#sizes(fields: Fields, count: number, payload: number): number[] {
		if (count === 0) {
			if (payload > 0) throw new Error(`LOCMAF chunk has no samples but ${payload} mdat bytes`);
			return [];
		}

		let sizes: number[];
		const listed = fields.get(SAMPLE_SIZES) as number[] | undefined;
		const size = fields.get(DEFAULT_SAMPLE_SIZE) as number | undefined;
		if (listed) {
			const sum = listed.reduce((total, size) => total + size, 0);
			if (sum > payload) throw new Error(`LOCMAF sample sizes sum to ${sum}, past the ${payload} mdat bytes`);
			sizes = [...listed, payload - sum];
		} else if (size === undefined && count === 1) {
			sizes = [payload];
		} else {
			// The header's default, else trex's when set, else zero for an empty mdat.
			const uniform = size ?? (this.#init.defaultSampleSize || (payload === 0 ? 0 : undefined));
			if (uniform === undefined) throw new Error(`LOCMAF chunk of ${count} samples carries no sample size`);
			if (count * uniform !== payload) {
				throw new Error(`LOCMAF chunk of ${count} samples of ${uniform} bytes has ${payload} mdat bytes`);
			}
			sizes = new Array(count).fill(uniform);
		}

		// A media decoder has nothing to do with an empty sample, which only an event track carries.
		if (sizes.some((size) => size === 0)) throw new Error("LOCMAF chunk has an empty sample");
		return sizes;
	}
}
