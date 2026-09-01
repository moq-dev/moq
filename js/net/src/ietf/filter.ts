/**
 * The Location Filter carried by SUBSCRIBE, and draft-20's fill request.
 *
 * @module
 */

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
	return version === Version.DRAFT_20;
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

/** Encode a filter as its raw parameter value. Unfiltered encodes to zero bytes. */
export function encode(filter: Filter, version: IetfVersion): Uint8Array {
	return isDraft20(version) ? encodeFields(filter) : encodeTag(filter);
}

function encodeFields(filter: Filter): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			return new Uint8Array();
		case "nextObject":
			return concat([Varint.encode(0n), Varint.encode(0n)]);
		case "relative":
			return Varint.encode(filter.groups);
		case "absolute": {
			// An open ended absolute {0, 0} is defined as equivalent to unfiltered, so it
			// normalizes rather than colliding with the two-zero-field nextObject spelling.
			if (filter.startGroup === 0n && filter.startObject === 0n && filter.endGroup === undefined) {
				return new Uint8Array();
			}
			const parts = [Varint.encode(filter.startGroup), Varint.encode(filter.startObject)];
			if (filter.endGroup !== undefined) {
				parts.push(Varint.encode(endDelta(filter.startGroup, filter.endGroup)));
				if (filter.endObject !== undefined) {
					parts.push(Varint.encode(filter.endObject));
				}
			}
			return concat(parts);
		}
	}
}

function encodeTag(filter: Filter): Uint8Array {
	switch (filter.kind) {
		case "unfiltered":
			// No tag means "everything", which only the absolute spelling can say.
			return concat([Varint.encode(TAG_ABSOLUTE_START), Varint.encode(0n), Varint.encode(0n)]);
		case "nextObject":
			return Varint.encode(TAG_LARGEST_OBJECT);
		case "relative":
			if (filter.groups !== 0n) {
				// Only draft-20 can name a start further back than the next group without
				// knowing Largest Object, so there is no honest tag to fall back to.
				throw new Error(`relative filter ${filter.groups} needs draft-20`);
			}
			return Varint.encode(TAG_NEXT_GROUP);
		case "absolute": {
			// Draft-19's AbsoluteRange ends on a group, so an object-bounded range has no
			// spelling. Refuse rather than widen the range the caller asked for.
			if (filter.endObject !== undefined) {
				throw new Error("an object-bounded range needs draft-20");
			}
			const parts = [
				Varint.encode(filter.endGroup === undefined ? TAG_ABSOLUTE_START : TAG_ABSOLUTE_RANGE),
				Varint.encode(filter.startGroup),
				Varint.encode(filter.startObject),
			];
			if (filter.endGroup !== undefined) {
				parts.push(Varint.encode(endDelta(filter.startGroup, filter.endGroup)));
			}
			return concat(parts);
		}
	}
}

/** Decode a filter from its raw parameter value. */
export function decode(data: Uint8Array, version: IetfVersion): Filter {
	return isDraft20(version) ? decodeFields(data) : decodeTag(data);
}

function decodeFields(data: Uint8Array): Filter {
	const fields: bigint[] = [];
	let rest = data;
	while (rest.length > 0) {
		if (fields.length === 4) {
			throw new Error("too many fields in LOCATION_FILTER");
		}
		const [value, next] = Varint.decodeBigInt(rest);
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
	const [tag, afterTag] = Varint.decodeBigInt(data);

	const readLocation = (buf: Uint8Array): [bigint, bigint, Uint8Array] => {
		const [group, afterGroup] = Varint.decodeBigInt(buf);
		const [object, rest] = Varint.decodeBigInt(afterGroup);
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
			const [delta, rest] = Varint.decodeBigInt(afterLocation);
			expectEmpty(rest);
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

/** LOCATION_FILTER, the only parameter we act on inside a fill. */
const FILL_LOCATION_FILTER = 0x21n;

/**
 * The parameters draft-20 allows inside FILL_PARAMETERS. Anything else is a protocol
 * violation rather than something to skip.
 */
/**
 * Maps each to whether it carries a length prefix.
 *
 * The Key-Value-Pair rule keys the framing off the id's parity, but the Range Filters
 * (0x25-0x28) are written with an explicit `Length` field despite two of them having even
 * ids. Their own definition wins, so the framing is tabulated rather than derived: reading
 * 0x26 or 0x28 as a bare varint would desync every parameter after it.
 */
const FILL_ALLOWED = new Map<bigint, boolean>([
	[0x0an, false], // FILL_TIMEOUT, a varint
	[0x20n, false], // SUBSCRIBER_PRIORITY, a uint8 varint
	[FILL_LOCATION_FILTER, true],
	[0x22n, false], // GROUP_ORDER, a uint8 varint
	[0x25n, true], // SUBGROUP_FILTER
	[0x26n, true], // OBJECTID_FILTER, length prefixed despite an even id
	[0x27n, true], // PRIORITY_FILTER
	[0x28n, true], // OBJECT_PROPERTY_FILTER, likewise
]);

/**
 * Encode FILL_PARAMETERS, whose presence is what requests a backfill.
 *
 * The value is a nested parameter scope, encoded like a message's parameters.
 */
export function encodeFill(filter: Filter, version: IetfVersion): Uint8Array {
	if (filter.kind === "unfiltered") {
		// An empty scope fills the whole track up to Largest Object.
		return Varint.encode(0n);
	}
	return concat([
		Varint.encode(1n),
		// The first type in a scope is not delta encoded, so this is the raw id.
		Varint.encode(FILL_LOCATION_FILTER),
		encodeLengthPrefixed(encode(filter, version)),
	]);
}

/** Decode FILL_PARAMETERS, returning the range it asks to fill. */
export function decodeFill(data: Uint8Array, version: IetfVersion): Filter {
	const [count, afterCount] = Varint.decodeBigInt(data);
	if (count > 64n) {
		throw new Error("too many parameters in FILL_PARAMETERS");
	}

	let rest = afterCount;
	let filter: Filter | undefined;
	let prev = 0n;
	for (let i = 0n; i < count; i++) {
		const [delta, afterType] = Varint.decodeBigInt(rest);
		const key = i === 0n ? delta : prev + delta;
		prev = key;
		rest = afterType;

		const lengthPrefixed = FILL_ALLOWED.get(key);
		if (lengthPrefixed === undefined) {
			throw new Error(`parameter ${key} is not allowed inside FILL_PARAMETERS`);
		}

		// The rest are parameters we do not act on, but their bytes still have to be
		// consumed or the remaining keys desync.
		if (!lengthPrefixed) {
			[, rest] = Varint.decodeBigInt(rest);
			continue;
		}

		const [length, afterLength] = Varint.decodeBigInt(rest);
		if (BigInt(afterLength.length) < length) {
			throw new Error("truncated value inside FILL_PARAMETERS");
		}
		const value = afterLength.slice(0, Number(length));
		rest = afterLength.slice(Number(length));

		if (key === FILL_LOCATION_FILTER) {
			if (filter !== undefined) {
				throw new Error("duplicate LOCATION_FILTER inside FILL_PARAMETERS");
			}
			filter = decode(value, version);
		}
	}

	if (rest.length !== 0) {
		throw new Error("trailing bytes in FILL_PARAMETERS");
	}

	return filter ?? { kind: "unfiltered" };
}

function encodeLengthPrefixed(value: Uint8Array): Uint8Array {
	return concat([Varint.encode(BigInt(value.length)), value]);
}
