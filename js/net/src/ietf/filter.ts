/**
 * The Location Filter carried by SUBSCRIBE, and draft-22's fill request.
 *
 * @module
 */

import { Milli } from "@moq/clock";
import { ProtocolViolation } from "../error.ts";
import { concat, type Reader } from "../stream.ts";
import type { Order } from "../track.ts";
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

/** The tagged Filter Type of draft-18 and earlier. */
const TAG_NEXT_GROUP = 0x1n;
const TAG_LARGEST_OBJECT = 0x2n;
const TAG_ABSOLUTE_START = 0x3n;
const TAG_ABSOLUTE_RANGE = 0x4n;

/**
 * Whether this is draft-20 or newer.
 *
 * Draft-20 replaced the Filter Type tag with up to four optional varints, where the number
 * present selects the meaning, and added the parameters that ride with it. Draft-22 names
 * the meaning with a Location Filter Type instead, but keeps those parameters.
 */
export function isDraft20(version: IetfVersion): boolean {
	return version !== Version.DRAFT_18;
}

/**
 * Whether LOCATION_FILTER starts with a Location Filter Type, which says which fields follow,
 * so the value carries no Length. Draft-22 replaced draft-20's field count with it.
 */
function isTyped(version: IetfVersion): boolean {
	return isDraft20(version) && version !== Version.DRAFT_20;
}

/** The Location Filter Type of draft-22. */
const TYPE_NONE = 0x0n;
const TYPE_RELATIVE_START = 0x1n;
const TYPE_ABSOLUTE_START = 0x2n;
const TYPE_ABSOLUTE_GROUP_END = 0x3n;
const TYPE_ABSOLUTE_RANGE = 0x4n;
const TYPE_NEXT_OBJECT = 0x5n;

/** Whether the Range Filters exist on this draft. */
export function hasRangeFilters(version: IetfVersion): boolean {
	return isDraft20(version);
}

const varint = Varint.encodeLeadingOnes;
const unvarint = Varint.decodeLeadingOnes;

function endDelta(startGroup: bigint, endGroup: bigint): bigint {
	if (endGroup < startGroup) {
		throw new Error(`filter range runs backwards: ${startGroup} > ${endGroup}`);
	}
	return endGroup - startGroup;
}

/** Encode a filter as its raw parameter value, without the Length that frames it before draft-22. */
export function encode(filter: Filter, version: IetfVersion): Uint8Array {
	if (isTyped(version)) return encodeTyped(filter);
	return isDraft20(version) ? encodeFields(filter) : encodeTag(filter);
}

/** Encode LOCATION_FILTER's value as it follows the parameter type from draft-17 on. */
export function encodeParam(filter: Filter, version: IetfVersion): Uint8Array {
	const value = encode(filter, version);
	// Draft-22's Location Filter Type says where the value ends, so it needs no Length.
	return isTyped(version) ? value : encodeLengthPrefixed(value);
}

/** Read LOCATION_FILTER's value as it follows the parameter type from draft-17 on. */
export async function decodeParam(r: Reader, version: IetfVersion): Promise<Filter> {
	if (!isTyped(version)) {
		return decode(await r.read(await r.u53()), version);
	}
	const type = await r.u62();
	const fields: bigint[] = [];
	for (let i = typedFieldCount(type); i > 0; i--) {
		fields.push(await r.u62());
	}
	return fromTyped(type, fields);
}

/** Encode the draft-20 field list, where the number of fields selects the meaning. */
function encodeFields(filter: Filter): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			return new Uint8Array();
		case "nextObject":
			return concat([varint(0n), varint(0n)]);
		case "relative":
			return varint(filter.groups);
		case "absolute": {
			// An open ended {0, 0} is defined as unfiltered, so it must not collide with Next Object.
			if (filter.startGroup === 0n && filter.startObject === 0n && filter.endGroup === undefined) {
				return new Uint8Array();
			}
			const parts = [varint(filter.startGroup), varint(filter.startObject)];
			if (filter.endGroup !== undefined) {
				parts.push(varint(endDelta(filter.startGroup, filter.endGroup)));
				if (filter.endObject !== undefined) {
					parts.push(varint(filter.endObject));
				}
			}
			return concat(parts);
		}
	}
}

function encodeTyped(filter: Filter): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			return varint(TYPE_NONE);
		case "nextObject":
			return varint(TYPE_NEXT_OBJECT);
		case "relative":
			return concat([varint(TYPE_RELATIVE_START), varint(filter.groups)]);
		case "absolute": {
			const type =
				filter.endGroup === undefined
					? TYPE_ABSOLUTE_START
					: filter.endObject === undefined
						? TYPE_ABSOLUTE_GROUP_END
						: TYPE_ABSOLUTE_RANGE;
			const parts = [varint(type), varint(filter.startGroup), varint(filter.startObject)];
			if (filter.endGroup !== undefined) {
				parts.push(varint(endDelta(filter.startGroup, filter.endGroup)));
				if (filter.endObject !== undefined) {
					parts.push(varint(filter.endObject));
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
function readTyped(data: Uint8Array): [Filter, Uint8Array] {
	let [type, rest] = unvarint(data);
	const fields: bigint[] = [];
	for (let i = typedFieldCount(type); i > 0; i--) {
		const [field, next] = unvarint(rest);
		fields.push(field);
		rest = next;
	}
	return [fromTyped(type, fields), rest];
}

function encodeTag(filter: Filter): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			// No tag means "everything", which only the absolute spelling can say.
			return concat([varint(TAG_ABSOLUTE_START), varint(0n), varint(0n)]);
		case "nextObject":
			return varint(TAG_LARGEST_OBJECT);
		case "relative":
			if (filter.groups !== 0n) {
				// Only draft-22 can name a start further back than the next group without
				// knowing Largest Object, so there is no honest tag to fall back to.
				throw new Error(`relative filter ${filter.groups} needs draft-22`);
			}
			return varint(TAG_NEXT_GROUP);
		case "absolute": {
			// Draft-18's AbsoluteRange ends on a group, so an object-bounded range has no
			// spelling. Refuse rather than widen the range the caller asked for.
			if (filter.endObject !== undefined) {
				throw new Error("an object-bounded range needs draft-22");
			}
			const parts = [
				varint(filter.endGroup === undefined ? TAG_ABSOLUTE_START : TAG_ABSOLUTE_RANGE),
				varint(filter.startGroup),
				varint(filter.startObject),
			];
			if (filter.endGroup !== undefined) {
				parts.push(varint(endDelta(filter.startGroup, filter.endGroup)));
			}
			return concat(parts);
		}
	}
}

/** Decode a filter from its raw parameter value, which must be consumed whole. */
export function decode(data: Uint8Array, version: IetfVersion): Filter {
	if (isTyped(version)) {
		const [filter, rest] = readTyped(data);
		expectEmpty(rest);
		return filter;
	}
	return isDraft20(version) ? decodeFields(data) : decodeTag(data);
}

/** Decode the draft-20 field list, which the caller has already delimited. */
function decodeFields(data: Uint8Array): Filter {
	const fields: bigint[] = [];
	let rest = data;
	while (rest.length > 0) {
		if (fields.length === 4) {
			throw new Error("too many fields in LOCATION_FILTER");
		}
		const [value, next] = unvarint(rest);
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

function decodeTag(data: Uint8Array): Filter {
	const [tag, afterTag] = unvarint(data);

	const readLocation = (buf: Uint8Array): [bigint, bigint, Uint8Array] => {
		const [group, afterGroup] = unvarint(buf);
		const [object, rest] = unvarint(afterGroup);
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
			const [delta, rest] = unvarint(afterLocation);
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
 * A draft-22 fill: the backfill a subscriber asks for alongside its subscription.
 *
 * The publisher serves the retained groups on a fetch stream in the requested order,
 * capped at the Largest Object snapshot independently of the subscription's live cursor.
 */
export interface Fill {
	/** The filled groups' order, or the subscription's order when omitted. */
	groupOrder?: Order;

	/** FILL_TIMEOUT, bounding the fill's wait as it does a FETCH's; see {@link Parameters.fillTimeout}. */
	fillTimeout?: Milli;

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

/** LOCATION_FILTER, the parameter that selects the fill's objects. */
const FILL_LOCATION_FILTER = 0x21n;
/** FILL_TIMEOUT, the fill's budget for waiting on objects not yet available. */
const FILL_TIMEOUT = 0x0an;

type Framing = "byte" | "bytes";

/**
 * The parameters draft-22 allows inside FILL_PARAMETERS besides LOCATION_FILTER and
 * FILL_TIMEOUT, and how each frames its value.
 *
 * Tabulated rather than derived, because neither shortcut is right. The Key-Value-Pair rule
 * keys framing off the id's parity, but the Range Filters (0x25-0x28) carry an explicit
 * Length despite two of them having even ids. And a uint8 parameter is one raw byte rather
 * than a varint, so reading it as one misparses any value with a leading 1-bit. Either
 * mistake desyncs every parameter after it.
 */
const FILL_ALLOWED = new Map<bigint, Framing>([
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
	const count = [fill.fillTimeout, fill.filter, fill.groupOrder].filter((v) => v !== undefined).length;
	const parts = [varint(BigInt(count))];
	// Parameters sort by id, each a delta from the one before.
	let prev = 0n;
	const key = (id: bigint) => {
		parts.push(varint(id - prev));
		prev = id;
	};
	if (fill.fillTimeout !== undefined) {
		key(FILL_TIMEOUT);
		parts.push(varint(BigInt(fill.fillTimeout)));
	}
	if (fill.filter !== undefined) {
		key(FILL_LOCATION_FILTER);
		parts.push(encodeParam(fill.filter, version));
	}
	if (fill.groupOrder !== undefined) {
		key(0x22n);
		parts.push(new Uint8Array([fill.groupOrder === "ascending" ? 1 : 2]));
	}
	return concat(parts);
}

/** Decode FILL_PARAMETERS, including its inherited or explicit group order. */
export function decodeFill(data: Uint8Array, version: IetfVersion): Fill {
	const [count, afterCount] = unvarint(data);
	if (count > 64n) {
		throw new Error("too many parameters in FILL_PARAMETERS");
	}

	let rest = afterCount;
	let filter: Filter | undefined;
	let groupOrder: Order | undefined;
	let fillTimeout: Milli | undefined;
	let rangeFilters = false;
	let prev = 0n;
	for (let i = 0n; i < count; i++) {
		const [delta, afterType] = unvarint(rest);
		const key = i === 0n ? delta : prev + delta;
		prev = key;
		rest = afterType;

		// Its framing depends on the draft, so the filter reads itself.
		if (key === FILL_LOCATION_FILTER) {
			if (filter !== undefined) {
				throw new Error("duplicate LOCATION_FILTER inside FILL_PARAMETERS");
			}
			if (isTyped(version)) {
				[filter, rest] = readTyped(rest);
			} else {
				let value: Uint8Array;
				[value, rest] = readLengthPrefixed(rest);
				filter = decode(value, version);
			}
			continue;
		}

		if (key === FILL_TIMEOUT) {
			if (fillTimeout !== undefined) throw new Error("duplicate FILL_TIMEOUT inside FILL_PARAMETERS");
			let value: bigint;
			[value, rest] = unvarint(rest);
			// Saturated like a FETCH's, since no wait lasts that long anyway.
			fillTimeout = Milli(value > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(value));
			continue;
		}

		const framing = FILL_ALLOWED.get(key);
		if (framing === undefined) {
			throw new Error(`parameter ${key} is not allowed inside FILL_PARAMETERS`);
		}

		// A Range Filter changes which objects the fill may contain, so its presence is
		// recorded even though its value is not interpreted.
		rangeFilters ||= key >= 0x25n && key <= 0x28n;

		// Each parameter must consume its own value or the remaining keys desync.
		if (framing === "byte") {
			if (rest.length < 1) throw new Error("truncated value inside FILL_PARAMETERS");
			if (key === 0x22n) {
				if (groupOrder !== undefined) throw new Error("duplicate GROUP_ORDER inside FILL_PARAMETERS");
				if (rest[0] !== 1 && rest[0] !== 2) {
					throw new ProtocolViolation(`invalid group order: ${rest[0]}`);
				}
				groupOrder = rest[0] === 1 ? "ascending" : "descending";
			}
			rest = rest.slice(1);
			continue;
		}

		[, rest] = readLengthPrefixed(rest);
	}

	if (rest.length !== 0) {
		throw new Error("trailing bytes in FILL_PARAMETERS");
	}

	return { filter, groupOrder, fillTimeout, rangeFilters };
}

/** Split a length-prefixed value off the front of `data`, returning it and what follows. */
function readLengthPrefixed(data: Uint8Array): [Uint8Array, Uint8Array] {
	const [length, afterLength] = unvarint(data);
	if (BigInt(afterLength.length) < length) {
		throw new Error("truncated value inside FILL_PARAMETERS");
	}
	return [afterLength.slice(0, Number(length)), afterLength.slice(Number(length))];
}

function encodeLengthPrefixed(value: Uint8Array): Uint8Array {
	return concat([varint(BigInt(value.length)), value]);
}
