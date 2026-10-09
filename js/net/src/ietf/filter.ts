/**
 * The Location Filter carried by SUBSCRIBE, and draft-20's fill request.
 *
 * @module
 */

import { ProtocolViolation } from "../error.ts";
import type { Reader } from "../stream.ts";
import * as Varint from "../varint.ts";
import { type IetfVersion, Version } from "./version.ts";

/** Which Objects a subscription delivers. */
export type Filter =
	/** Every Object in the track, encoded by omitting the parameter. */
	| { kind: "unfiltered" }
	/** The next Object after the live edge, which can begin mid-group. */
	| { kind: "nextObject" }
	/**
	 * The given number of groups back from the next group, always open ended.
	 * Zero is the next group and one is the current one.
	 */
	| { kind: "relative"; groups: bigint }
	/**
	 * An absolute range. `endGroup` is the last group, inclusive; `endObject` further bounds
	 * the last object in it, and its absence includes the whole end group.
	 */
	| { kind: "absolute"; startGroup: bigint; startObject: bigint; endGroup?: bigint; endObject?: bigint };

/** The tagged Filter Type of draft-19 and earlier. */
const TAG_NEXT_GROUP = 0x1n;
const TAG_LARGEST_OBJECT = 0x2n;
const TAG_ABSOLUTE_START = 0x3n;
const TAG_ABSOLUTE_RANGE = 0x4n;

/**
 * Whether this is draft-20 or newer.
 *
 * Draft-20 replaced the Filter Type tag with up to four optional varints, where the number
 * present selects the meaning.
 */
export function isDraft20(version: IetfVersion): boolean {
	return (
		version !== Version.DRAFT_14 &&
		version !== Version.DRAFT_15 &&
		version !== Version.DRAFT_16 &&
		version !== Version.DRAFT_17 &&
		version !== Version.DRAFT_18 &&
		version !== Version.DRAFT_19
	);
}

/**
 * Whether this is draft-22 or newer.
 *
 * Draft-22 replaced the length-inferred field list with a Location Filter Type that names
 * the fields that follow, so the value carries no Length.
 */
export function isDraft22(version: IetfVersion): boolean {
	return isDraft20(version) && version !== Version.DRAFT_20 && version !== Version.DRAFT_21;
}

/** The Location Filter Type of draft-22. */
const TYPE_NONE = 0x0n;
const TYPE_RELATIVE_START = 0x1n;
const TYPE_ABSOLUTE_START = 0x2n;
const TYPE_ABSOLUTE_GROUP_END = 0x3n;
const TYPE_ABSOLUTE_RANGE = 0x4n;
const TYPE_NEXT_OBJECT = 0x5n;

/** Whether the Range Filters (draft-19) exist on this draft. */
export function hasRangeFilters(version: IetfVersion): boolean {
	return isDraft20(version) || version === Version.DRAFT_19;
}

/**
 * Draft-17 replaced QUIC's two-bit-length varint with a leading-1-bits one. The two agree
 * below 64 and diverge above it, so getting this wrong is invisible until group or object
 * ids grow past that.
 */
function usesLeadingOnes(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16;
}

function varint(v: bigint, version: IetfVersion): Uint8Array {
	return usesLeadingOnes(version) ? Varint.encodeLeadingOnes(v) : Varint.encode(v);
}

function unvarint(buf: Uint8Array, version: IetfVersion): [bigint, Uint8Array] {
	return usesLeadingOnes(version) ? Varint.decodeLeadingOnes(buf) : Varint.decodeBigInt(buf);
}

function concat(parts: Uint8Array[]): Uint8Array {
	const total = parts.reduce((n, p) => n + p.length, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

function endDelta(startGroup: bigint, endGroup: bigint): bigint {
	if (endGroup < startGroup) {
		throw new Error(`filter range runs backwards: ${startGroup} > ${endGroup}`);
	}
	return endGroup - startGroup;
}

/**
 * Encode a filter as its raw parameter value, without the Length that frames it through
 * draft-21. Unfiltered encodes to zero bytes on draft-20 and draft-21.
 */
export function encode(filter: Filter, version: IetfVersion): Uint8Array {
	if (isDraft22(version)) return encodeTyped(filter, version);
	return isDraft20(version) ? encodeFields(filter, version) : encodeTag(filter, version);
}

/** Encode LOCATION_FILTER's value as it follows the parameter type from draft-17 on. */
export function encodeParam(filter: Filter, version: IetfVersion): Uint8Array {
	const value = encode(filter, version);
	// Draft-22's Location Filter Type says where the value ends, so it needs no Length.
	return isDraft22(version) ? value : encodeLengthPrefixed(value, version);
}

/** Read LOCATION_FILTER's value as it follows the parameter type from draft-17 on. */
export async function decodeParam(r: Reader, version: IetfVersion): Promise<Filter> {
	if (!isDraft22(version)) {
		return decode(await r.read(await r.u53()), version);
	}
	const type = await r.u62();
	const fields: bigint[] = [];
	for (let i = typedFieldCount(type); i > 0; i--) {
		fields.push(await r.u62());
	}
	return fromTyped(type, fields);
}

function encodeTyped(filter: Filter, version: IetfVersion): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			return varint(TYPE_NONE, version);
		case "nextObject":
			return varint(TYPE_NEXT_OBJECT, version);
		case "relative":
			return concat([varint(TYPE_RELATIVE_START, version), varint(filter.groups, version)]);
		case "absolute": {
			const type =
				filter.endGroup === undefined
					? TYPE_ABSOLUTE_START
					: filter.endObject === undefined
						? TYPE_ABSOLUTE_GROUP_END
						: TYPE_ABSOLUTE_RANGE;
			const parts = [
				varint(type, version),
				varint(filter.startGroup, version),
				varint(filter.startObject, version),
			];
			if (filter.endGroup !== undefined) {
				parts.push(varint(endDelta(filter.startGroup, filter.endGroup), version));
				if (filter.endObject !== undefined) {
					parts.push(varint(filter.endObject, version));
				}
			}
			return concat(parts);
		}
	}
}

/** How many fields follow a draft-22 Location Filter Type. */
function typedFieldCount(type: bigint): number {
	switch (type) {
		case TYPE_NONE:
		case TYPE_NEXT_OBJECT:
			return 0;
		case TYPE_RELATIVE_START:
			return 1;
		case TYPE_ABSOLUTE_START:
			return 2;
		case TYPE_ABSOLUTE_GROUP_END:
			return 3;
		case TYPE_ABSOLUTE_RANGE:
			return 4;
		default:
			throw new ProtocolViolation(`unknown Location Filter Type: ${type}`);
	}
}

/** Build a filter from a draft-22 Location Filter Type and the fields it named. */
function fromTyped(type: bigint, fields: bigint[]): Filter {
	switch (type) {
		case TYPE_NONE:
			return { kind: "unfiltered" };
		case TYPE_NEXT_OBJECT:
			return { kind: "nextObject" };
		case TYPE_RELATIVE_START:
			return { kind: "relative", groups: fields[0] };
		default: {
			const [startGroup, startObject, delta, endObject] = fields;
			const endGroup = delta === undefined ? undefined : startGroup + delta;
			if (endGroup !== undefined && endGroup > 0xffff_ffff_ffff_ffffn) {
				throw new ProtocolViolation("LOCATION_FILTER end group exceeds 2^64 - 1");
			}
			return { kind: "absolute", startGroup, startObject, endGroup, endObject };
		}
	}
}

/** Read a draft-22 filter from the front of `data`, returning what follows it. */
function readTyped(data: Uint8Array, version: IetfVersion): [Filter, Uint8Array] {
	let [type, rest] = unvarint(data, version);
	const fields: bigint[] = [];
	for (let i = typedFieldCount(type); i > 0; i--) {
		const [field, next] = unvarint(rest, version);
		fields.push(field);
		rest = next;
	}
	return [fromTyped(type, fields), rest];
}

function encodeFields(filter: Filter, version: IetfVersion): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			return new Uint8Array();
		case "nextObject":
			return concat([varint(0n, version), varint(0n, version)]);
		case "relative":
			return varint(filter.groups, version);
		case "absolute": {
			// An open ended absolute {0, 0} is defined as equivalent to unfiltered, so it
			// normalizes rather than colliding with the two-zero-field nextObject spelling.
			if (filter.startGroup === 0n && filter.startObject === 0n && filter.endGroup === undefined) {
				return new Uint8Array();
			}
			const parts = [varint(filter.startGroup, version), varint(filter.startObject, version)];
			if (filter.endGroup !== undefined) {
				parts.push(varint(endDelta(filter.startGroup, filter.endGroup), version));
				if (filter.endObject !== undefined) {
					parts.push(varint(filter.endObject, version));
				}
			}
			return concat(parts);
		}
	}
}

function encodeTag(filter: Filter, version: IetfVersion): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			// No tag means "everything", which only the absolute spelling can say.
			return concat([varint(TAG_ABSOLUTE_START, version), varint(0n, version), varint(0n, version)]);
		case "nextObject":
			return varint(TAG_LARGEST_OBJECT, version);
		case "relative":
			if (filter.groups !== 0n) {
				// Only draft-20 can name a start further back than the next group without
				// knowing Largest Object, so there is no honest tag to fall back to.
				throw new Error(`relative filter ${filter.groups} needs draft-20`);
			}
			return varint(TAG_NEXT_GROUP, version);
		case "absolute": {
			// Draft-19's AbsoluteRange ends on a group, so an object-bounded range has no
			// spelling. Refuse rather than widen the range the caller asked for.
			if (filter.endObject !== undefined) {
				throw new Error("an object-bounded range needs draft-20");
			}
			const parts = [
				varint(filter.endGroup === undefined ? TAG_ABSOLUTE_START : TAG_ABSOLUTE_RANGE, version),
				varint(filter.startGroup, version),
				varint(filter.startObject, version),
			];
			if (filter.endGroup !== undefined) {
				parts.push(varint(endDelta(filter.startGroup, filter.endGroup), version));
			}
			return concat(parts);
		}
	}
}

/** Decode a filter from its raw parameter value, which must be consumed whole. */
export function decode(data: Uint8Array, version: IetfVersion): Filter {
	if (isDraft22(version)) {
		const [filter, rest] = readTyped(data, version);
		expectEmpty(rest);
		return filter;
	}
	return isDraft20(version) ? decodeFields(data, version) : decodeTag(data, version);
}

function decodeFields(data: Uint8Array, version: IetfVersion): Filter {
	const fields: bigint[] = [];
	let rest = data;
	while (rest.length > 0) {
		if (fields.length === 4) {
			throw new Error("too many fields in LOCATION_FILTER");
		}
		const [value, next] = unvarint(rest, version);
		fields.push(value);
		rest = next;
	}

	switch (fields.length) {
		case 0:
			return { kind: "unfiltered" };
		case 1:
			return { kind: "relative", groups: fields[0] };
		default: {
			const [startGroup, startObject] = fields;
			// Two zeroes is the Next Object spelling; anything else is an absolute start.
			if (fields.length === 2 && startGroup === 0n && startObject === 0n) {
				return { kind: "nextObject" };
			}
			const endGroup = fields.length >= 3 ? startGroup + fields[2] : undefined;
			const endObject = fields.length === 4 ? fields[3] : undefined;
			return { kind: "absolute", startGroup, startObject, endGroup, endObject };
		}
	}
}

function decodeTag(data: Uint8Array, version: IetfVersion): Filter {
	const [tag, afterTag] = unvarint(data, version);

	const readLocation = (buf: Uint8Array): [bigint, bigint, Uint8Array] => {
		const [group, afterGroup] = unvarint(buf, version);
		const [object, rest] = unvarint(afterGroup, version);
		return [group, object, rest];
	};

	switch (tag) {
		case TAG_NEXT_GROUP:
			expectEmpty(afterTag);
			return { kind: "relative", groups: 0n };
		case TAG_LARGEST_OBJECT:
			expectEmpty(afterTag);
			return { kind: "nextObject" };
		case TAG_ABSOLUTE_START: {
			const [startGroup, startObject, rest] = readLocation(afterTag);
			expectEmpty(rest);
			return { kind: "absolute", startGroup, startObject };
		}
		case TAG_ABSOLUTE_RANGE: {
			const [startGroup, startObject, afterLocation] = readLocation(afterTag);
			const [delta, rest] = unvarint(afterLocation, version);
			expectEmpty(rest);
			return { kind: "absolute", startGroup, startObject, endGroup: startGroup + delta };
		}
		default:
			throw new Error(`unsupported filter type: ${tag}`);
	}
}

/**
 * Read the tagged filter draft-14 carries inline in a SUBSCRIBE body, where the fields
 * following the tag have no length to delimit them.
 *
 * Every tag is read, including the absolute forms we do not serve. Rejecting one would tear
 * the session down over a filter the draft lets a subscriber send; what we do with it is
 * decided when the range is resolved.
 */
export async function decodeInline(r: Reader): Promise<Filter> {
	const tag = await r.u62();
	switch (tag) {
		case TAG_NEXT_GROUP:
			return { kind: "relative", groups: 0n };
		case TAG_LARGEST_OBJECT:
			return { kind: "nextObject" };
		case TAG_ABSOLUTE_START:
			return { kind: "absolute", startGroup: await r.u62(), startObject: await r.u62() };
		case TAG_ABSOLUTE_RANGE: {
			const startGroup = await r.u62();
			const startObject = await r.u62();
			const delta = await r.u62();
			return { kind: "absolute", startGroup, startObject, endGroup: startGroup + delta };
		}
		default:
			throw new Error(`unsupported filter type: ${tag}`);
	}
}

function expectEmpty(rest: Uint8Array): void {
	if (rest.length !== 0) {
		throw new Error("trailing bytes in LOCATION_FILTER");
	}
}

/**
 * A draft-20 fill: the backfill a subscriber asks for alongside its subscription.
 *
 * The publisher serves one whose range resolves to a single group, straight from the group
 * cache on a fetch stream. That covers the draft's own current-group join (a Next Object
 * subscription plus a `StartGroup=1` fill). Anything wider is refused by resetting the
 * fetch stream, the draft's fill-failure signal.
 */
export interface Fill {
	/**
	 * The range to fill. `undefined` means the Location Filter was omitted, which inherits
	 * the subscription's own filter; `unfiltered` (a zero length filter, or type 0x00 from
	 * draft-22) means the whole track up to Largest Object.
	 */
	filter?: Filter;

	/**
	 * Whether the scope carried a Range Filter (0x25-0x28). Those narrow which objects pass,
	 * which we do not implement, and serving the unfiltered range instead would deliver
	 * objects the peer excluded. Never encoded; we send no range filters.
	 */
	rangeFilters: boolean;
}

/** LOCATION_FILTER, the only parameter we act on inside a fill. */
const FILL_LOCATION_FILTER = 0x21n;

type Framing = "byte" | "varint" | "bytes";

/**
 * The parameters draft-20 allows inside FILL_PARAMETERS besides LOCATION_FILTER, and how
 * each frames its value.
 *
 * Tabulated rather than derived, because neither shortcut is right. The Key-Value-Pair rule
 * keys framing off the id's parity, but the Range Filters (0x25-0x28) carry an explicit
 * Length despite two of them having even ids. And a uint8 parameter is one raw byte rather
 * than a varint, so reading it as one misparses any value with a leading 1-bit. Either
 * mistake desyncs every parameter after it.
 */
const FILL_ALLOWED = new Map<bigint, Framing>([
	[0x0an, "varint"], // FILL_TIMEOUT
	[0x20n, "byte"], // SUBSCRIBER_PRIORITY, a uint8
	[0x22n, "byte"], // GROUP_ORDER, a uint8
	[0x25n, "bytes"], // SUBGROUP_FILTER
	[0x26n, "bytes"], // OBJECTID_FILTER, length prefixed despite an even id
	[0x27n, "bytes"], // PRIORITY_FILTER
	[0x28n, "bytes"], // OBJECT_PROPERTY_FILTER, likewise
]);

/**
 * Encode FILL_PARAMETERS, whose presence is what requests a backfill.
 *
 * The value is a nested parameter scope, encoded like a message's parameters.
 */
export function encodeFill(fill: Fill, version: IetfVersion): Uint8Array {
	// An omitted filter inherits the subscription's, so the scope is empty. An explicit
	// unfiltered still encodes, as a zero length filter (type 0x00 from draft-22) meaning
	// the whole track.
	if (fill.filter === undefined) {
		return varint(0n, version);
	}
	return concat([
		varint(1n, version),
		// The first type in a scope is not delta encoded, so this is the raw id.
		varint(FILL_LOCATION_FILTER, version),
		encodeParam(fill.filter, version),
	]);
}

/** Decode FILL_PARAMETERS, returning the range it asks to fill. */
export function decodeFill(data: Uint8Array, version: IetfVersion): Fill {
	const [count, afterCount] = unvarint(data, version);
	if (count > 64n) {
		throw new Error("too many parameters in FILL_PARAMETERS");
	}

	let rest = afterCount;
	let filter: Filter | undefined;
	let rangeFilters = false;
	let prev = 0n;
	for (let i = 0n; i < count; i++) {
		const [delta, afterType] = unvarint(rest, version);
		const key = i === 0n ? delta : prev + delta;
		prev = key;
		rest = afterType;

		// Its framing depends on the draft, so the filter reads itself.
		if (key === FILL_LOCATION_FILTER) {
			if (filter !== undefined) {
				throw new Error("duplicate LOCATION_FILTER inside FILL_PARAMETERS");
			}
			if (isDraft22(version)) {
				[filter, rest] = readTyped(rest, version);
			} else {
				let value: Uint8Array;
				[value, rest] = readLengthPrefixed(rest, version);
				filter = decode(value, version);
			}
			continue;
		}

		const framing = FILL_ALLOWED.get(key);
		if (framing === undefined) {
			throw new Error(`parameter ${key} is not allowed inside FILL_PARAMETERS`);
		}

		// A Range Filter changes which objects the fill may contain, so its presence is
		// recorded even though its value is not interpreted.
		rangeFilters ||= key >= 0x25n && key <= 0x28n;

		// The rest are parameters we do not act on, but their bytes still have to be
		// consumed or the remaining keys desync.
		if (framing === "varint") {
			[, rest] = unvarint(rest, version);
			continue;
		}
		if (framing === "byte") {
			if (rest.length < 1) throw new Error("truncated value inside FILL_PARAMETERS");
			rest = rest.slice(1);
			continue;
		}

		[, rest] = readLengthPrefixed(rest, version);
	}

	if (rest.length !== 0) {
		throw new Error("trailing bytes in FILL_PARAMETERS");
	}

	return { filter, rangeFilters };
}

/** Split a length-prefixed value off the front of `data`, returning it and what follows. */
function readLengthPrefixed(data: Uint8Array, version: IetfVersion): [Uint8Array, Uint8Array] {
	const [length, afterLength] = unvarint(data, version);
	if (BigInt(afterLength.length) < length) {
		throw new Error("truncated value inside FILL_PARAMETERS");
	}
	return [afterLength.slice(0, Number(length)), afterLength.slice(Number(length))];
}

function encodeLengthPrefixed(value: Uint8Array, version: IetfVersion): Uint8Array {
	return concat([varint(BigInt(value.length), version), value]);
}
