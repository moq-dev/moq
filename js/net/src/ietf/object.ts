import { ProtocolViolation, StreamCode, Stream as StreamError } from "../error.ts";
import { decodePayload } from "../payload.ts";
import type { Cursor, Reader } from "../stream.ts";
import { asIetf, concat, encodeVarint, Writer } from "../stream.ts";
import { type Timescale, Timestamp } from "../time.ts";
import { U64 } from "../util/u64.ts";
import { DEFAULT_WIRE } from "./priority.ts";
import { type IetfVersion, isDraft23, Version } from "./version.ts";

// Implementation limit for object extension blocks, independent of the IETF draft.
const MAX_OBJECT_EXTENSIONS = 64 * 1024;

const GROUP_END = 0x03;
const END_OF_TRACK = 0x04;

/** Whether `status` is an Object Status the drafts define: Normal, End of Group, or End of Track. */
export function statusDefined(status: number): boolean {
	return status === 0 || status === GROUP_END || status === END_OF_TRACK;
}

// MOQ Object Property ids. Both are even, so their values are bare varints.
const PRIOR_GROUP_ID_GAP = 0x3cn;
const TIMESTAMP_TIE = 0x40b72n;
const IMMUTABLE_PROPERTIES = 0x0bn;

// draft-18 adds bit 0x40 (FIRST_OBJECT) to the subgroup header type per spec
// 11.4.2. Set on emit unless a filter trimmed the group's head. On parse a
// cleared bit is the peer's claim that the stream starts partway through;
// object 0 is still the payload head; a standalone terminal may name a later ID.
const FIRST_OBJECT_BIT = 0x40;

const MAX_SAFE_KEY = BigInt(Number.MAX_SAFE_INTEGER);

/**
 * Compute the wire `(id, tie)` for a frame at `dts` in group `groupId` following `prior`
 * (the previous frame's Object ID, if any), per draft-lcurley-moq-timestamp.
 */
export function timestampObjectId(
	groupId: bigint,
	prior: bigint | undefined,
	dts: bigint,
): { id: bigint; tie: bigint } {
	const minOid = prior === undefined ? 0n : prior + 1n;
	const desired = dts > groupId ? dts - groupId : 0n;
	let id = minOid;
	if (desired > id) id = desired;
	const tie = groupId + id > dts ? groupId + id - dts : 0n;
	return { id, tie };
}

/**
 * A received object's timestamp: `Group ID + Object ID - tie` in the track's `ID_TIMESCALE`
 * when declared (per draft-lcurley-moq-timestamp), else {@link Timestamp.unscaled}.
 */
export function objectTime(timescale: Timescale | undefined, key: bigint, objectId = 0n, tie = 0n): Timestamp {
	if (timescale === undefined) return Timestamp.unscaled(key);
	const dts = key + objectId - tie;
	if (dts < 0n) {
		throw new StreamError(StreamCode.MalformedTrack, {
			message: `TIMESTAMP_TIE ${tie} exceeds ${key} + ${objectId}`,
		});
	}
	if (dts > MAX_SAFE_KEY) throw new RangeError(`timestamp ${dts} is past the latest timestamp`);
	return new Timestamp(Number(dts), timescale);
}

/** Decoded known object properties from a Properties block. */
export interface DecodedObjectProperties {
	groupGap?: bigint;
	tie?: bigint;
}

/** Encode the Properties block of an object carrying an optional Prior Group ID Gap and/or Timestamp Tie. */
export function encodeObjectProperties(
	{ groupGap, tie }: { groupGap?: bigint; tie?: bigint },
	version: IetfVersion,
): Uint8Array | undefined {
	const parts: Uint8Array[] = [];
	let prev = 0n;
	if (groupGap !== undefined && groupGap > 0n) {
		parts.push(encodeVarint(PRIOR_GROUP_ID_GAP - prev, version), encodeVarint(groupGap, version));
		prev = PRIOR_GROUP_ID_GAP;
	}
	if (tie !== undefined && tie > 0n) {
		parts.push(encodeVarint(TIMESTAMP_TIE - prev, version), encodeVarint(tie, version));
	}
	return parts.length > 0 ? concat(parts) : undefined;
}

/** Encode the Properties block of an object naming `gap` Group IDs just below its own as never produced. */
export function encodeGroupGap(gap: bigint, version: IetfVersion): Uint8Array {
	return concat([encodeVarint(PRIOR_GROUP_ID_GAP, version), encodeVarint(gap, version)]);
}

/**
 * Decode an object's Properties block, consuming all of it, for the Prior Group ID Gap and
 * Timestamp Tie, outside or inside Immutable Properties. Every other property is skipped.
 */
export function decodeObjectProperties(c: Cursor): DecodedObjectProperties {
	return decodeProperties(c, false);
}

/**
 * Decode an object's Properties block, consuming all of it, for the Prior Group ID Gap, outside
 * or inside Immutable Properties. Every other property is skipped.
 */
export function decodeGroupGap(c: Cursor): bigint | undefined {
	return decodeProperties(c, false).groupGap;
}

function decodeProperties(c: Cursor, immutable: boolean): DecodedObjectProperties {
	let groupGap: bigint | undefined;
	let tie: bigint | undefined;
	let sealed = false;
	let prevType = 0n;
	const malformed = (message: string) => new StreamError(StreamCode.MalformedTrack, { message });
	while (c.remaining > 0) {
		const id = prevType + c.u62();
		prevType = id;

		if (id % 2n === 0n) {
			const value = c.u62();
			if (id === PRIOR_GROUP_ID_GAP) {
				if (groupGap !== undefined) throw malformed("repeated Prior Group ID Gap");
				groupGap = value;
			} else if (id === TIMESTAMP_TIE) {
				if (tie !== undefined) throw malformed("repeated Timestamp Tie");
				tie = value;
			}
			continue;
		}
		const length = c.u53();
		if (id !== IMMUTABLE_PROPERTIES) {
			c.read(length);
			continue;
		}
		if (immutable || sealed) throw malformed("nested or repeated Immutable Properties");
		sealed = true;
		let inner: DecodedObjectProperties;
		try {
			inner = c.exact(length, (e) => decodeProperties(e, true));
		} catch (err: unknown) {
			if (err instanceof StreamError) throw err;
			throw malformed("unreadable Immutable Properties");
		}
		if (inner.groupGap !== undefined) {
			if (groupGap !== undefined) throw malformed("repeated Prior Group ID Gap");
			groupGap = inner.groupGap;
		}
		if (inner.tie !== undefined) {
			if (tie !== undefined) throw malformed("repeated Timestamp Tie");
			tie = inner.tie;
		}
	}
	return { groupGap, tie };
}

export interface GroupFlags {
	hasExtensions: boolean;
	hasSubgroup: boolean;
	hasSubgroupObject: boolean;
	hasEnd: boolean;
	// v15: whether priority is present in the header.
	// When false (0x30 base), priority inherits from the control message.
	hasPriority: boolean;
	/**
	 * Whether this stream carries the subgroup from its first published object.
	 *
	 * A subgroup that starts partway through has a hole at the front, which moq-lite cannot
	 * represent. Always true on the drafts that predate the FIRST_OBJECT bit (before
	 * draft-18), where there is no such signal to read.
	 */
	firstObject: boolean;
}

/**
 * STREAM_HEADER_SUBGROUP from moq-transport spec.
 * Used for stream-per-group delivery mode.
 */
export class Group {
	flags: GroupFlags;
	trackAlias: bigint;
	groupId: bigint;
	subGroupId: bigint;
	publisherPriority: number;

	constructor({
		trackAlias,
		groupId,
		subGroupId,
		publisherPriority,
		flags,
	}: {
		trackAlias: bigint;
		groupId: bigint;
		subGroupId: bigint;
		publisherPriority: number;
		flags: GroupFlags;
	}) {
		this.flags = flags;
		this.trackAlias = trackAlias;
		this.groupId = groupId;
		this.subGroupId = subGroupId;
		this.publisherPriority = publisherPriority;
	}

	/**
	 * Write the header, stream type first, in one write: a peer that classifies a stream from
	 * its first read (imquic) drops a header split across several.
	 */
	async encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.write(await Writer.collect(version, (h) => this.#encode(h, version)));
	}

	async #encode(w: Writer, _version: IetfVersion): Promise<void> {
		if (!this.flags.hasSubgroup && this.subGroupId !== 0n) {
			throw new Error(`Subgroup ID must be 0 if hasSubgroup is false: ${this.subGroupId}`);
		}

		const base = this.flags.hasPriority ? 0x10 : 0x30;
		let id = base;
		if (this.flags.hasExtensions) {
			id |= 0x01;
		}
		if (this.flags.hasSubgroupObject) {
			id |= 0x02;
		}
		if (this.flags.hasSubgroup) {
			id |= 0x04;
		}
		if (this.flags.hasEnd) {
			id |= 0x08;
		}
		if (this.flags.firstObject) {
			id |= FIRST_OBJECT_BIT;
		}
		await w.u53(id);
		await w.u62(this.trackAlias);
		await w.u62(this.groupId);
		if (this.flags.hasSubgroup) {
			await w.u62(this.subGroupId);
		}
		if (this.flags.hasPriority) {
			await w.u8(this.publisherPriority);
		}
	}

	/** Decode a SUBGROUP_HEADER. Pass `type` when the caller already read it to classify the stream. */
	static async decode(r: Reader, _version: IetfVersion, type?: number): Promise<Group> {
		const raw = type ?? (await r.u53());
		if (raw > 0x7f) throw new ProtocolViolation(`Unsupported group type: ${raw}`);
		// Strip the FIRST_OBJECT bit before the range check, but keep the value: it is the only
		// signal that a subgroup starts partway through.
		const firstObject = (raw & FIRST_OBJECT_BIT) !== 0;
		const id = raw & ~FIRST_OBJECT_BIT;

		// An invalid type MUST close the session, including the reserved SUBGROUP_ID_MODE
		// 0b11 (draft-22 section 11.3.1).
		const known = (id >= 0x10 && id <= 0x1f) || (id >= 0x30 && id <= 0x3f);
		if (!known || (id & 0x06) === 0x06) {
			throw new ProtocolViolation(`Unsupported group type: ${raw}`);
		}

		// 0x30-0x3F omit the priority and inherit it from the control message.
		const hasPriority = id < 0x30;
		const baseId = hasPriority ? id : id - (0x30 - 0x10);

		const flags: GroupFlags = {
			hasExtensions: (baseId & 0x01) !== 0,
			hasSubgroupObject: (baseId & 0x02) !== 0,
			hasSubgroup: (baseId & 0x04) !== 0,
			hasEnd: (baseId & 0x08) !== 0,
			hasPriority,
			firstObject,
		};

		const trackAlias = await r.u62();
		const groupId = await r.u62();
		const subGroupId = flags.hasSubgroup ? await r.u62() : 0n;
		const publisherPriority = hasPriority ? await r.u8() : DEFAULT_WIRE;

		return new Group({ trackAlias, groupId, subGroupId, publisherPriority, flags });
	}
}

/**
 * An object id that is not the next one in the group.
 *
 * moq-lite groups start at object 0 and never skip one, so a non-zero delta is either
 * a subgroup that starts partway through or a hole later in the stream.
 */
export class ObjectIdGap extends Error {
	constructor(delta: number | U64) {
		super(`object IDs must start at 0 and increment by 1, got a delta of ${delta}`);
		this.name = "ObjectIdGap";
	}
}

/** A draft-14 END_OF_TRACK carried properties, which a track-level end has nowhere to keep. */
export class EndOfTrackProperties extends Error {
	constructor() {
		super("END_OF_TRACK properties are unsupported");
		this.name = "EndOfTrackProperties";
	}
}

/** A moq-transport object inside a group stream. */
export class Frame {
	/**
	 * The object payload, or `undefined` for an end of group or end of track marker, or for
	 * an object whose payload streams after its header (see {@link streamed}).
	 */
	payload?: Uint8Array;
	/** The payload length of an object past one segment, which follows on the stream instead of in {@link payload}. */
	streamed?: number;
	/**
	 * The Prior Group ID Gap in the object's properties: how many Group IDs just below its own
	 * the publisher will never produce. Only the group's first object sends one.
	 */
	groupGap?: bigint;
	/** The Timestamp Tie (`0x40B72`) subtracted from `Group ID + Object ID`. */
	tie?: bigint;
	/**
	 * An END_OF_TRACK marker: no object at or past its location exists. At object 0 its group
	 * does not exist either, so the track ends at that group; later in a group it ends after it.
	 */
	endOfTrack: boolean;
	/** Absolute object ID for the first object or a standalone terminal; delta minus 1 thereafter. */
	idDelta = 0;
	/** An END_OF_TRACK that carries properties, which a track end has nowhere to keep. */
	hasProperties = false;

	constructor({
		payload,
		streamed,
		groupGap,
		tie,
		endOfTrack = false,
	}: { payload?: Uint8Array; streamed?: number; groupGap?: bigint; tie?: bigint; endOfTrack?: boolean } = {}) {
		this.payload = payload;
		this.streamed = streamed;
		this.groupGap = groupGap;
		this.tie = tie;
		this.endOfTrack = endOfTrack;
	}

	/** Whether this is an object, rather than an end of group or end of track marker. */
	get isObject(): boolean {
		return this.payload !== undefined || this.streamed !== undefined;
	}

	/**
	 * Encode this frame using the group flags and negotiated IETF version.
	 *
	 * `idDelta` is the first object's absolute Object ID and the gap above `prior + 1` for
	 * every later one. A {@link streamed} object writes only its header; the caller writes its payload.
	 */
	async encode(w: Writer, flags: GroupFlags, version: IetfVersion, idDelta: number | bigint = 0): Promise<void> {
		// Everything before the payload is built in memory and goes out as one write: a stream
		// write is far costlier than a varint, and a backlog pays it for every object.
		const header = [encodeVarint(idDelta, version)];

		if (flags.hasExtensions) {
			const properties = encodeObjectProperties({ groupGap: this.groupGap, tie: this.tie }, version);
			header.push(encodeVarint(properties?.byteLength ?? 0, version));
			if (properties) header.push(properties);
		}

		const payload = this.endOfTrack ? undefined : this.payload;
		if (this.endOfTrack) {
			header.push(encodeVarint(0, version), encodeVarint(END_OF_TRACK, version)); // length = 0
		} else if (this.streamed) {
			header.push(encodeVarint(this.streamed, version));
		} else if (payload === undefined) {
			header.push(encodeVarint(0, version), encodeVarint(GROUP_END, version)); // length = 0
		} else {
			header.push(encodeVarint(payload.byteLength, version));
			if (payload.byteLength === 0) header.push(encodeVarint(0, version)); // status = normal
		}

		await w.write(concat(header));
		if (payload !== undefined && payload.byteLength > 0) await w.write(payload);
	}

	/**
	 * Decode a frame using the group flags, at the cursor's negotiated IETF version.
	 *
	 * `offset` lets a stream's first object start past zero, reporting its absolute Object ID
	 * in `idDelta` rather than throwing {@link ObjectIdGap}. `sparse` allows arbitrary non-zero
	 * Object ID deltas on a timed track (draft-lcurley-moq-timestamp).
	 */
	static decode(c: Cursor, flags: GroupFlags, offset = false, sparse = false): Frame {
		let delta = 0;
		if (flags.hasSubgroupObject) {
			// A compact stream's first Object ID is its subgroup, and only subgroup 0 is
			// supported, so refuse one of any size before the rest of the object can fail
			// differently. Later objects carry plain deltas: decode them without this flag.
			const id = c.varint();
			if (!id.equals(U64.ZERO)) throw new ObjectIdGap(id);
		} else {
			delta = c.u53();
		}

		let groupGap: bigint | undefined;
		let tie: bigint | undefined;
		let hasProperties = false;
		if (flags.hasExtensions) {
			const extensionsLength = c.u53();
			if (extensionsLength > MAX_OBJECT_EXTENSIONS) {
				throw new StreamError(StreamCode.MalformedTrack, { message: "object extensions exceed 64 KiB" });
			}
			hasProperties = extensionsLength !== 0;
			const decoded = c.exact(extensionsLength, decodeObjectProperties);
			groupGap = decoded.groupGap;
			tie = decoded.tie;
		}

		const payloadLength = c.u53();

		if (payloadLength > 0) {
			if (delta !== 0 && !offset && !sparse) throw new ObjectIdGap(delta);
			const payload = decodePayload(c, payloadLength);
			const frame = new Frame({
				payload,
				streamed: payload === undefined ? payloadLength : undefined,
				groupGap,
				tie,
			});
			frame.idDelta = delta;
			return frame;
		}

		const status = c.u53();
		if (status !== 0 && hasProperties) {
			throw new ProtocolViolation(`status object ${status} carries properties`);
		}

		// Defined on every implemented draft, whether or not the header marks the group's end.
		if (status === END_OF_TRACK) {
			const frame = new Frame({ endOfTrack: true });
			frame.idDelta = delta;
			frame.hasProperties = hasProperties;
			return frame;
		}
		if (delta !== 0 && !((offset || sparse) && (status === 0 || status === GROUP_END))) {
			throw new ObjectIdGap(delta);
		}
		// Allowed even when the header marks the group's end: that bit only lets a FIN imply it,
		// and imquic sends both.
		if (status === GROUP_END) {
			const frame = new Frame();
			frame.idDelta = delta;
			return frame;
		}

		if (status === 0) {
			const frame = new Frame({ payload: new Uint8Array(0), groupGap, tie });
			frame.idDelta = delta;
			return frame;
		}

		throw new Error(`Unsupported object status: ${status}`);
	}
}

// Serialization Flags for a fetch object: the two low bits encode the subgroup (00 =
// subgroup zero), then one presence bit per field.
const FETCH_OBJECT_ID = 0x04;
const FETCH_GROUP_ID = 0x08;
const FETCH_PRIORITY = 0x10;
const FETCH_PROPERTIES = 0x20;

/** Where a {@link FetchFrame} sits in the track, and whether it opens the stream. */
export interface FetchPosition {
	/** The group's sequence number. */
	group: bigint;
	/** The object's id within that group. */
	object: number;
	/**
	 * Whether this is the stream's first object, which carries the priority the rest inherit,
	 * and its absolute ids unless an End of Range came before it.
	 */
	first: boolean;
	/** The group of the prior object or End of Range when this object starts a different group. */
	previousGroup?: bigint;
	/** The Object ID of the prior object in the same group, when skipping IDs. */
	previousObject?: number;
}

/**
 * A moq-transport object inside a fetch stream, which a publisher writes to serve a fill.
 *
 * The first object carries absolute Group and Object IDs plus the priority. A new group
 * carries its delta and absolute Object ID; subsequent objects inherit the group and
 * increment the Object ID. A zero payload length is simply an empty object.
 */
export class FetchFrame {
	/** The object payload. */
	payload: Uint8Array;
	/** The payload length when the caller streams its bytes after the header. */
	streamed?: number;
	/**
	 * The track's Publisher Priority on the wire, which the stream's first object states for
	 * every object after it, as a subgroup header does.
	 */
	priority: number;
	/** Optional Timestamp Tie (`0x40B72`) property. */
	tie?: bigint;

	constructor({
		payload,
		streamed,
		priority,
		tie,
	}: { payload?: Uint8Array; streamed?: number; priority: number; tie?: bigint }) {
		this.payload = payload ?? new Uint8Array(0);
		this.streamed = streamed;
		this.priority = priority;
		this.tie = tie;
	}

	/** Encode this object at `position`, with optional properties. */
	async encode(w: Writer, position: FetchPosition, version: IetfVersion): Promise<void> {
		const previous = position.previousGroup;
		const properties = encodeObjectProperties({ tie: this.tie }, version);
		const propFlag = properties ? FETCH_PROPERTIES : 0;
		// Everything before the payload goes out as one write, as in Frame.encode.
		const header: Uint8Array[] = [];
		if (position.first || previous !== undefined) {
			// Include the priority too: "same as the prior object" has no prior to refer to.
			header.push(
				encodeVarint(
					FETCH_GROUP_ID | FETCH_OBJECT_ID | (position.first ? FETCH_PRIORITY : 0) | propFlag,
					version,
				),
				encodeVarint(
					previous === undefined
						? position.group
						: (position.group > previous ? position.group - previous : previous - position.group) - 1n,
					version,
				),
				encodeVarint(position.object, version),
			);
			if (position.first) header.push(Uint8Array.of(this.priority));
		} else {
			const step = position.previousObject === undefined ? 1 : position.object - position.previousObject;
			if (step > 1) {
				header.push(
					encodeVarint(FETCH_OBJECT_ID | propFlag, version),
					encodeVarint(step - objectDeltaBias(version), version),
				);
			} else {
				// Same group and priority; the Object ID is the prior one plus one.
				header.push(encodeVarint(propFlag, version));
			}
		}

		if (properties) {
			header.push(encodeVarint(properties.byteLength, version), properties);
		}

		header.push(encodeVarint(this.streamed ?? this.payload.byteLength, version));
		await w.write(concat(header));
		if (this.payload.byteLength > 0) {
			await w.write(this.payload);
		}
	}
}

/** Why the Locations a {@link FetchEndOfRange} closes are left out of a fetch stream. */
export const EndOfRange = {
	/** They do not exist. */
	NonExistent: 0x8c,
	/** Their state is unknown, which before draft-22 includes a FILL_TIMEOUT running out. */
	Unknown: 0x10c,
	/** The FILL_TIMEOUT ran out before they arrived, from draft-22. */
	TimedOut: 0x20c,
	/** They are gone, from draft-23. */
	Gone: 0x40c,
} as const;

/** A reason {@link EndOfRange} names. */
export type EndOfRange = (typeof EndOfRange)[keyof typeof EndOfRange];

/**
 * An End of Range on a fetch stream: every Location after the prior object, through this
 * one, is missing for its reason.
 *
 * Through draft-22 its Group and Object IDs are absolute, wherever it falls: read as deltas,
 * the Group ID could never name the group it follows, and every implementation reads them as
 * absolute (moq-wg/moq-transport#1861). Draft-23 makes them deltas from the prior entry after
 * the stream's first (section 11.4.2.1); this writes the fields as given. Nothing follows them.
 */
export class FetchEndOfRange {
	reason: EndOfRange;
	group: bigint;
	object: bigint;

	constructor({ reason, group, object }: { reason: EndOfRange; group: bigint; object: bigint }) {
		this.reason = reason;
		this.group = group;
		this.object = object;
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		if (this.reason === EndOfRange.TimedOut && version === Version.DRAFT_18) {
			throw new Error("End of Timed-Out Range needs draft-22");
		}
		if (this.reason === EndOfRange.Gone && !hasGroupEnd(version)) {
			throw new Error("End of Gone Range needs draft-23");
		}
		await w.u53(this.reason);
		await w.u62(this.group);
		await w.u62(this.object);
	}
}

const END_OF_RANGE: readonly number[] = [EndOfRange.NonExistent, EndOfRange.Unknown, EndOfRange.TimedOut];

// Draft-23's indicators: the prior object ends its group, or the track (section 11.4.2.2).
const FETCH_END_OF_GROUP = 0x80;
const FETCH_END_OF_TRACK = 0x81;

/**
 * Whether the fetch stream has draft-23's End of Group, End of Track and End of Gone Range
 * (section 11.4.1), and reads its End of Range IDs as deltas (section 11.4.2.1).
 */
export function hasGroupEnd(version: IetfVersion): boolean {
	return isDraft23(version);
}

/**
 * How far past its own value an Object ID Delta reaches within a group: draft-23 (section
 * 11.4.1) adds two, so a delta is only sent for a gap. Earlier drafts add the delta alone.
 */
export function objectDeltaBias(version: IetfVersion): number {
	return isDraft23(version) ? 2 : 0;
}

// Serialization Flags at or above this are named values, not a set of flags.
const FETCH_NAMED = 0x80;
// The flag of an object published as a datagram, whose low bits then mean nothing.
const FETCH_DATAGRAM = 0x40;
// The low bits, which spell the Subgroup ID.
const FETCH_SUBGROUP = 0x03;

/** How a fetched object names its Subgroup ID. */
type FetchSubgroup = "zero" | "prior" | "next" | "explicit" | "datagram";

/** One object read off a fetch stream, the inverse of {@link FetchFrame.encode}. */
export type FetchedObject =
	| {
			kind: "object";
			/** The Group ID field: absolute on the first object, a delta to a different group after. */
			group?: bigint;
			/** How the Subgroup ID is spelled. */
			subgroup: FetchSubgroup;
			/** The explicit Subgroup ID, for `subgroup: "explicit"`. */
			subgroupId?: bigint;
			/** The Object ID field: absolute with `group`, otherwise a delta from the prior one. */
			object?: number;
			/** The Publisher Priority, absent when it repeats the prior object's. */
			priority?: number;
			/** Optional Timestamp Tie (`0x40B72`) decoded from object properties. */
			tie?: bigint;
			/** The object payload; empty for an empty object, and `undefined` when it streams after the object. */
			payload?: Uint8Array;
			/** The payload length; past one segment, the payload follows on the stream. */
			size: number;
	  }
	/** The Group and Object ID fields: absolute, or from draft-23 deltas after the first entry. */
	| { kind: "endOfRange"; reason: EndOfRange; group: bigint; object: bigint }
	/** Draft-23's End of Group, or with `track` its End of Track. */
	| { kind: "endOfGroup"; track: boolean };

/** Decode the next object on a fetch stream, extracting Timestamp Tie when present. */
export function decodeFetchObject(c: Cursor): FetchedObject {
	// Any 62-bit value is a legal field, so an unknown one is a violation, not a decode failure.
	const wide = c.varint();
	if (wide.hi !== 0 || wide.lo >= FETCH_NAMED) {
		const version = asIetf(c.version);
		const named = version !== undefined && hasGroupEnd(version);
		if (wide.hi === 0 && named && (wide.lo === FETCH_END_OF_GROUP || wide.lo === FETCH_END_OF_TRACK)) {
			return { kind: "endOfGroup", track: wide.lo === FETCH_END_OF_TRACK };
		}
		if (wide.hi !== 0 || (!END_OF_RANGE.includes(wide.lo) && !(named && wide.lo === EndOfRange.Gone))) {
			throw new ProtocolViolation(`unknown fetch serialization flags: 0x${wide.toBigInt().toString(16)}`);
		}
		return { kind: "endOfRange", reason: wide.lo as EndOfRange, group: c.u62(), object: c.u62() };
	}
	const flags = wide.lo;

	// Wire order: Group ID, Subgroup ID, Object ID, Priority, Properties.
	const group = flags & FETCH_GROUP_ID ? c.u62() : undefined;
	let subgroup: FetchSubgroup;
	let subgroupId: bigint | undefined;
	if (flags & FETCH_DATAGRAM) {
		subgroup = "datagram";
	} else {
		switch (flags & FETCH_SUBGROUP) {
			case 0:
				subgroup = "zero";
				break;
			case 1:
				subgroup = "prior";
				break;
			case 2:
				subgroup = "next";
				break;
			default:
				subgroup = "explicit";
				subgroupId = c.u62();
		}
	}
	const object = flags & FETCH_OBJECT_ID ? c.u53() : undefined;
	const priority = flags & FETCH_PRIORITY ? c.u8() : undefined;

	let tie: bigint | undefined;
	if (flags & FETCH_PROPERTIES) {
		const length = c.u53();
		if (length > MAX_OBJECT_EXTENSIONS) {
			throw new StreamError(StreamCode.MalformedTrack, { message: "object extensions exceed 64 KiB" });
		}
		tie = c.exact(length, decodeObjectProperties).tie;
	}

	// A fetch stream carries no Object Status: a zero length is simply an empty object.
	const length = c.u53();
	const payload = decodePayload(c, length);
	return { kind: "object", group, subgroup, subgroupId, object, priority, tie, payload, size: length };
}
