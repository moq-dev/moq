import { Reader, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { type IetfVersion, Version } from "./version.ts";

const GROUP_END = 0x03;
const PROP_TIMESTAMP = 0x06n;
const PROP_TIMESCALE = 0x08n;

// draft-18 adds bit 0x40 (FIRST_OBJECT) to the subgroup header type per spec
// 11.4.2. moq-lite always starts subgroups at object 0, so the bit carries no
// extra information for us: set it on emit, strip it on parse.
const FIRST_OBJECT_BIT = 0x40;

function hasFirstObjectBit(version: IetfVersion): boolean {
	switch (version) {
		case Version.DRAFT_14:
		case Version.DRAFT_15:
		case Version.DRAFT_16:
		case Version.DRAFT_17:
			return false;
		default:
			return true;
	}
}

function hasDeltaObjectPropertyTypes(version: IetfVersion | undefined): boolean {
	switch (version) {
		case Version.DRAFT_14:
		case Version.DRAFT_15:
			return false;
		default:
			return true;
	}
}

async function encodeObjectPropertyType(
	w: Writer,
	id: bigint,
	prev: bigint,
	version: IetfVersion | undefined,
): Promise<void> {
	const encoded = hasDeltaObjectPropertyTypes(version) ? id - prev : id;
	await w.u62(encoded);
}

async function encodeObjectTime(w: Writer, timestamp: Timestamp, version: IetfVersion | undefined): Promise<void> {
	await encodeObjectPropertyType(w, PROP_TIMESTAMP, 0n, version);
	await w.u62(BigInt(Math.round(timestamp.value)));
	await encodeObjectPropertyType(w, PROP_TIMESCALE, PROP_TIMESTAMP, version);
	await w.u62(BigInt(timestamp.scale));
}

async function encodeObjectExtensions(
	timestamp: Timestamp | undefined,
	version: IetfVersion | undefined,
): Promise<Uint8Array> {
	if (timestamp === undefined) {
		return new Uint8Array();
	}

	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		version,
	);
	await encodeObjectTime(writer, timestamp, version);
	writer.close();
	await writer.closed;

	const size = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
	const result = new Uint8Array(size);
	let offset = 0;
	for (const chunk of chunks) {
		result.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return result;
}

async function decodeObjectTime(r: Reader, version: IetfVersion | undefined): Promise<Timestamp | undefined> {
	let timestamp: bigint | undefined;
	let timescale: bigint | undefined;
	let prevType = 0n;
	let first = true;

	while (!(await r.done())) {
		const step = await r.u62();
		const id = !hasDeltaObjectPropertyTypes(version) || first ? step : prevType + step;
		first = false;
		prevType = id;

		if (id % 2n === 0n) {
			const value = await r.u62();
			if (id === PROP_TIMESTAMP) {
				timestamp = value;
			} else if (id === PROP_TIMESCALE) {
				timescale = value;
			}
		} else {
			const size = await r.u53();
			await r.read(size);
		}
	}

	if (timestamp === undefined) {
		return undefined;
	}

	return new Timestamp(Number(timestamp), Timescale(Number(timescale ?? BigInt(Timescale.MICRO))));
}

export interface GroupFlags {
	hasExtensions: boolean;
	hasSubgroup: boolean;
	hasSubgroupObject: boolean;
	hasEnd: boolean;
	// v15: whether priority is present in the header.
	// When false (0x30 base), priority inherits from the control message.
	hasPriority: boolean;
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

	async encode(w: Writer, version: IetfVersion): Promise<void> {
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
		if (hasFirstObjectBit(version)) {
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

	static async decode(r: Reader, version: IetfVersion): Promise<Group> {
		const raw = await r.u53();
		// Strip the draft-18 FIRST_OBJECT bit before the range check.
		const id = hasFirstObjectBit(version) ? raw & ~FIRST_OBJECT_BIT : raw;

		let hasPriority: boolean;
		let baseId: number;
		if (id >= 0x10 && id <= 0x1f) {
			hasPriority = true;
			baseId = id;
		} else if (id >= 0x30 && id <= 0x3f) {
			hasPriority = false;
			baseId = id - (0x30 - 0x10);
		} else {
			throw new Error(`Unsupported group type: ${id}`);
		}

		const flags: GroupFlags = {
			hasExtensions: (baseId & 0x01) !== 0,
			hasSubgroupObject: (baseId & 0x02) !== 0,
			hasSubgroup: (baseId & 0x04) !== 0,
			hasEnd: (baseId & 0x08) !== 0,
			hasPriority,
		};

		const trackAlias = await r.u62();
		const groupId = await r.u53();
		const subGroupId = flags.hasSubgroup ? await r.u53() : 0;
		const publisherPriority = hasPriority ? await r.u8() : 128; // Default priority when absent

		return new Group({ trackAlias, groupId, subGroupId, publisherPriority, flags });
	}
}

/** A moq-transport object inside a group stream. */
export class Frame {
	/** The object payload, or `undefined` for the end of group marker. */
	payload?: Uint8Array;
	/** The presentation timestamp carried in object properties, when present. */
	timestamp?: Timestamp;

	constructor({ payload, timestamp }: { payload?: Uint8Array; timestamp?: Timestamp } = {}) {
		this.payload = payload;
		this.timestamp = timestamp;
	}

	/** Encode this frame using the group flags and negotiated IETF version. */
	async encode(w: Writer, flags: GroupFlags, version = w.version): Promise<void> {
		await w.u53(0); // id_delta = 0

		if (flags.hasExtensions) {
			const extensions = await encodeObjectExtensions(this.timestamp, version);
			await w.u53(extensions.byteLength);
			await w.write(extensions);
		}

		if (this.payload !== undefined) {
			await w.u53(this.payload.byteLength);

			if (this.payload.byteLength === 0) {
				await w.u53(0); // status = normal
			} else {
				await w.write(this.payload);
			}
		} else {
			await w.u53(0); // length = 0
			await w.u53(GROUP_END);
		}
	}

	/** Decode a frame using the group flags and negotiated IETF version. */
	static async decode(r: Reader, flags: GroupFlags, version = r.version): Promise<Frame> {
		const delta = await r.u53();
		if (delta !== 0) {
			throw new Error(`object ID delta is not supported: ${delta}`);
		}

		let timestamp: Timestamp | undefined;
		if (flags.hasExtensions) {
			const extensionsLength = await r.u53();
			const extensions = await r.read(extensionsLength);
			timestamp = await decodeObjectTime(new Reader(undefined, extensions, version), version);
		}

		const payloadLength = await r.u53();

		if (payloadLength > 0) {
			const payload = await r.read(payloadLength);
			return new Frame({ payload, timestamp });
		}

		const status = await r.u53();

		if (flags.hasEnd) {
			// Empty frame
			if (status === 0) return new Frame({ payload: new Uint8Array(0), timestamp });
		} else if (status === 0 || status === GROUP_END) {
			// TODO status === 0 should be an empty frame, but moq-rs seems to be sending it incorrectly on group end.
			return new Frame();
		}

		throw new Error(`Unsupported object status: ${status}`);
	}
}
