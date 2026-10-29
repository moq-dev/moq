import { ProtocolViolation, StreamCode, Stream as StreamError } from "../error.ts";
import type { Cursor, Reader } from "../stream.ts";
import { concat, encodeVarint, Writer } from "../stream.ts";
import { type Timescale, Timestamp } from "../time.ts";
import { U64 } from "../util/u64.ts";
import { DEFAULT_WIRE } from "./priority.ts";
import { type IetfVersion, Version } from "./version.ts";

// Implementation limit for object extension blocks, independent of the IETF draft.
const MAX_OBJECT_EXTENSIONS = 64 * 1024;

const GROUP_END = 0x03;
const END_OF_TRACK = 0x04;

// MOQ Object Property ids, shared with draft-ietf-moq-loc-04. An object-scope Timescale
// (0x08) is never applied: a track's timeline is its TIMESCALE Track Property alone.
const PROP_TIMESTAMP = 0x10n;

// draft-18 adds bit 0x40 (FIRST_OBJECT) to the subgroup header type per spec
// 11.4.2. Set on emit unless a filter trimmed the group's head. On parse a
// cleared bit is the peer's claim that the stream starts partway through;
// object 0 is still the payload head; a standalone terminal may name a later ID.
const FIRST_OBJECT_BIT = 0x40;

/**
 * Encode an object's Properties block: its Timestamp in `timescale` units, or nothing.
 *
 * Without units (`timescale` undefined) there is no Timestamp to write, even if the frame has one.
 * Synchronous, like the rest of an object's header: every object of a timed track pays for it.
 */
export function encodeObjectExtensions(
	timestamp: Timestamp | undefined,
	timescale: Timescale | undefined,
	version: IetfVersion,
): Uint8Array {
	if (timestamp === undefined || timescale === undefined) {
		return new Uint8Array();
	}

	// The units come from the track's TIMESCALE property, not from here: repeating the
	// timescale on every object would cost bytes per frame to restate something fixed for
	// the track's lifetime. The timestamp is converted into the track's units so the value
	// on the wire matches what the track declared. The shared conversion is exact at the same
	// scale, and refused before anything is written when the result leaves the range a
	// Timestamp, and so the receiver, can hold.
	const value = Math.round(timestamp.as(timescale));
	// The only property, so its type's delta from the previous one is from zero.
	return concat([encodeVarint(PROP_TIMESTAMP, version), encodeVarint(value, version)]);
}

/** Decode the Timestamp an object's Properties block carries, consuming the whole block. */
export function decodeObjectTime(c: Cursor, timescale: Timescale): Timestamp | undefined {
	let timestamp: bigint | undefined;
	let prevType = 0n;
	let first = true;

	while (c.remaining > 0) {
		const step = c.u62();
		const id = first ? step : prevType + step;
		first = false;
		prevType = id;

		if (id % 2n === 0n) {
			const value = c.u62();
			if (id === PROP_TIMESTAMP) {
				timestamp = value;
			}
		} else {
			c.read(c.u53());
		}
	}

	if (timestamp === undefined) {
		return undefined;
	}

	return new Timestamp(Number(timestamp), timescale);
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
	groupId: number;
	subGroupId: number;
	publisherPriority: number;

	constructor({
		trackAlias,
		groupId,
		subGroupId,
		publisherPriority,
		flags,
	}: {
		trackAlias: bigint;
		groupId: number;
		subGroupId: number;
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
		if (!this.flags.hasSubgroup && this.subGroupId !== 0) {
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
		await w.u53(this.groupId);
		if (this.flags.hasSubgroup) {
			await w.u53(this.subGroupId);
		}
		if (this.flags.hasPriority) {
			await w.u8(this.publisherPriority);
		}
	}

	/** Decode a SUBGROUP_HEADER. Pass `type` when the caller already read it to classify the stream. */
	static async decode(r: Reader, _version: IetfVersion, type?: number): Promise<Group> {
		const raw = type ?? (await r.u53());
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
		const groupId = await r.u53();
		const subGroupId = flags.hasSubgroup ? await r.u53() : 0;
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
	/** The object payload, or `undefined` for an end of group or end of track marker. */
	payload?: Uint8Array;
	/** The presentation timestamp carried in object properties, when present. */
	timestamp?: Timestamp;
	/**
	 * An END_OF_TRACK marker: no object at or past its location exists. At object 0 its group
	 * does not exist either, so the track ends at that group; later in a group it ends after it.
	 */
	endOfTrack: boolean;
	/** Absolute object ID for a standalone terminal; zero for sequential objects. */
	idDelta = 0;
	/** An END_OF_TRACK that carries properties, which a track end has nowhere to keep. */
	hasProperties = false;

	constructor({
		payload,
		timestamp,
		endOfTrack = false,
	}: { payload?: Uint8Array; timestamp?: Timestamp; endOfTrack?: boolean } = {}) {
		this.payload = payload;
		this.timestamp = timestamp;
		this.endOfTrack = endOfTrack;
	}

	/**
	 * Encode this frame using the group flags and negotiated IETF version.
	 *
	 * The timestamp goes out in `timescale`, the units the track declared, and not at all when
	 * it declared none. `idDelta` is the first object's absolute Object ID and zero for every
	 * later one, so a group whose head was trimmed by a filter still puts the true numbering
	 * on the wire.
	 */
	async encode(
		w: Writer,
		flags: GroupFlags,
		timescale: Timescale | undefined,
		version: IetfVersion,
		idDelta = 0,
	): Promise<void> {
		// Everything before the payload is built in memory and goes out as one write: a stream
		// write is far costlier than a varint, and a backlog pays it for every object.
		const header = [encodeVarint(idDelta, version)];

		if (flags.hasExtensions) {
			const extensions = encodeObjectExtensions(this.timestamp, timescale, version);
			header.push(encodeVarint(extensions.byteLength, version), extensions);
		}

		const payload = this.endOfTrack ? undefined : this.payload;
		if (this.endOfTrack) {
			header.push(encodeVarint(0, version), encodeVarint(END_OF_TRACK, version)); // length = 0
		} else if (payload === undefined) {
			header.push(encodeVarint(0, version), encodeVarint(GROUP_END, version)); // length = 0
		} else {
			header.push(encodeVarint(payload.byteLength, version));
			if (payload.byteLength === 0) header.push(encodeVarint(0, version)); // status = normal
		}

		await w.write(concat(header));
		if (payload !== undefined && payload.byteLength > 0) await w.write(payload);
	}

	/** Decode a frame using the group flags, at the cursor's negotiated IETF version. */
	static decode(c: Cursor, flags: GroupFlags, timescale: Timescale | undefined): Frame {
		// The first object's delta is its absolute Object ID; every later one is the prior ID
		// plus the delta plus one. moq-lite groups start at object 0 and never skip one, so
		// a sequential group is a zero delta throughout, and any other value means the group
		// either starts partway through or has a gap that would renumber the frames after it.
		let delta = 0;
		if (flags.hasSubgroupObject) {
			// A compact stream's first Object ID is its subgroup and any later delta a gap.
			// Neither is supported, so refuse one of any size before the rest of the object
			// can fail differently.
			const id = c.varint();
			if (!id.equals(U64.ZERO)) throw new ObjectIdGap(id);
		} else {
			delta = c.u53();
		}

		let timestamp: Timestamp | undefined;
		let hasProperties = false;
		if (flags.hasExtensions) {
			const extensionsLength = c.u53();
			if (extensionsLength > MAX_OBJECT_EXTENSIONS) {
				throw new StreamError(StreamCode.MalformedTrack, { message: "object extensions exceed 64 KiB" });
			}
			hasProperties = extensionsLength !== 0;
			// A track that declared no timescale is untimed, so its objects stay untimed even
			// if one carries a Timestamp: there are no units to read it in.
			if (timescale !== undefined) {
				timestamp = c.exact(extensionsLength, (e) => decodeObjectTime(e, timescale));
			} else {
				c.read(extensionsLength);
			}
		}

		const payloadLength = c.u53();

		if (payloadLength > 0) {
			if (delta !== 0) throw new ObjectIdGap(delta);
			const payload = c.read(payloadLength);
			return new Frame({ payload, timestamp });
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
		if (delta !== 0) throw new ObjectIdGap(delta);
		// Allowed even when the header marks the group's end: that bit only lets a FIN imply it,
		// and imquic sends both.
		if (status === GROUP_END) return new Frame();

		if (status === 0) return new Frame({ payload: new Uint8Array(0), timestamp });

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
	group: number;
	/** The object's id within that group. */
	object: number;
	/**
	 * Whether this is the stream's first object, which carries the priority the rest inherit,
	 * and its absolute ids unless an End of Range came before it.
	 */
	first: boolean;
	/** The group of the prior object or End of Range when this object starts a different group. */
	previousGroup?: number;
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
	/** The presentation timestamp carried in object properties, when present. */
	timestamp?: Timestamp;

	constructor({ payload, timestamp }: { payload: Uint8Array; timestamp?: Timestamp }) {
		this.payload = payload;
		this.timestamp = timestamp;
	}

	/**
	 * Encode this object at `position`, stamping it in the track's timescale, or not at all
	 * when the track declared none.
	 */
	async encode(
		w: Writer,
		position: FetchPosition,
		timescale: Timescale | undefined,
		version: IetfVersion,
	): Promise<void> {
		const stamped = this.timestamp !== undefined && timescale !== undefined;
		const previous = position.previousGroup;
		// Everything before the payload goes out as one write, as in Frame.encode.
		const header: Uint8Array[] = [];
		if (position.first || previous !== undefined) {
			// Include the priority too: "same as the prior object" has no prior to refer to.
			const properties = stamped ? FETCH_PROPERTIES : 0;
			header.push(
				encodeVarint(
					FETCH_GROUP_ID | FETCH_OBJECT_ID | (position.first ? FETCH_PRIORITY : 0) | properties,
					version,
				),
				encodeVarint(
					previous === undefined ? position.group : Math.abs(position.group - previous) - 1,
					version,
				),
				encodeVarint(position.object, version),
			);
			if (position.first) header.push(Uint8Array.of(0));
		} else {
			// Same group and priority; the Object ID is the prior one plus one.
			header.push(encodeVarint(stamped ? FETCH_PROPERTIES : 0, version));
		}

		// Omitted entirely rather than written empty when the object carries no timestamp:
		// the track declared no units, so there is no property to send.
		if (stamped) {
			const extensions = encodeObjectExtensions(this.timestamp, timescale, version);
			header.push(encodeVarint(extensions.byteLength, version), extensions);
		}

		header.push(encodeVarint(this.payload.byteLength, version));
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
} as const;

/** A reason {@link EndOfRange} names. */
export type EndOfRange = (typeof EndOfRange)[keyof typeof EndOfRange];

/**
 * An End of Range on a fetch stream: every Location after the prior object, through this
 * one, is missing for its reason.
 *
 * Its Group and Object IDs are absolute, wherever it falls. Read as deltas, the Group ID could
 * never name the group it follows, and every implementation reads them as absolute
 * (moq-wg/moq-transport#1861). Nothing follows them: no payload length.
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
		await w.u53(this.reason);
		await w.u62(this.group);
		await w.u62(this.object);
	}
}
