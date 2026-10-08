/**
 * OBJECT_DATAGRAM: one Object carried in a QUIC datagram (draft-14 section 10.3.1 through
 * draft-20 section 11.3.1).
 *
 * The model counterpart is {@link Datagram}, a single-frame group, so only an Object at ID 0
 * maps onto it. The Type is a set of flags on every draft; draft-14 lacks the
 * DEFAULT_PRIORITY bit and a status with an omitted Object ID. Mirrors the Rust
 * `ietf::ObjectDatagram`.
 *
 * @module
 */
import type { Datagram } from "../datagram.ts";
import { error, ProtocolViolation, reason } from "../error.ts";
import { type Cursor, encodeVarint, Reader } from "../stream.ts";
import type { Timescale } from "../time.ts";
import { encodeObjectExtensions } from "./object.ts";
import { type IetfVersion, Version } from "./version.ts";

// The bits of an OBJECT_DATAGRAM Type.
const PROPERTIES = 0x01;
const END_OF_GROUP = 0x02;
const ZERO_OBJECT_ID = 0x04;
const DEFAULT_PRIORITY = 0x08;
const STATUS = 0x20;
// Every defined bit. Anything else, including the reserved 0x10, is invalid.
const ALL = PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY | STATUS;

/** Whether `kind` is a Type this draft defines. */
function valid(kind: number, version: IetfVersion): boolean {
	if (version === Version.DRAFT_14) return kind <= 0x07 || kind === 0x20 || kind === 0x21;
	// A status cannot also end the group.
	return (kind & ~ALL) === 0 && !((kind & STATUS) !== 0 && (kind & END_OF_GROUP) !== 0);
}

/** Drafts 14-16 let any status carry Properties; later ones only a Normal Object. */
function legacy(version: IetfVersion): boolean {
	return version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16;
}

/** What follows an OBJECT_DATAGRAM's header: the payload, or the status of an Object without one. */
export type DatagramBody = { payload: Uint8Array } | { status: number };

/** A decoded OBJECT_DATAGRAM. */
export class ObjectDatagram {
	trackAlias: bigint;
	groupId: number;
	/** The Object ID, or `undefined` when the ZERO_OBJECT_ID bit omits it (Object 0). */
	objectId?: number;
	/** The Publisher Priority, or `undefined` to inherit the subscription's (draft-15+). */
	publisherPriority?: number;
	/** No Object past this one exists in the group. */
	endOfGroup: boolean;
	/** The Object Properties block without its length prefix, which carries the Timestamp. */
	properties?: Uint8Array;
	body: DatagramBody;

	constructor(props: {
		trackAlias: bigint;
		groupId: number;
		objectId?: number;
		publisherPriority?: number;
		endOfGroup: boolean;
		properties?: Uint8Array;
		body: DatagramBody;
	}) {
		this.trackAlias = props.trackAlias;
		this.groupId = props.groupId;
		this.objectId = props.objectId;
		this.publisherPriority = props.publisherPriority;
		this.endOfGroup = props.endOfGroup;
		this.properties = props.properties;
		this.body = props.body;
	}

	/** Encode one QUIC datagram's bytes. Throws on a combination this draft cannot carry. */
	encode(version: IetfVersion): Uint8Array {
		let kind = 0;
		if (this.properties !== undefined) kind |= PROPERTIES;
		if (this.endOfGroup) kind |= END_OF_GROUP;
		if (this.objectId === undefined) kind |= ZERO_OBJECT_ID;
		if (this.publisherPriority === undefined) kind |= DEFAULT_PRIORITY;
		if ("status" in this.body) kind |= STATUS;
		if (!valid(kind, version)) throw new Error(`OBJECT_DATAGRAM type 0x${kind.toString(16)} is invalid`);

		const parts = [encodeVarint(kind, version), encodeVarint(this.trackAlias, version)];
		parts.push(encodeVarint(this.groupId, version));
		if (this.objectId !== undefined) parts.push(encodeVarint(this.objectId, version));
		if (this.publisherPriority !== undefined) parts.push(Uint8Array.of(this.publisherPriority));
		if (this.properties !== undefined) {
			// A present but empty block is a protocol violation for the peer.
			if (this.properties.byteLength === 0) throw new Error("OBJECT_DATAGRAM properties are empty");
			parts.push(encodeVarint(this.properties.byteLength, version), this.properties);
		}
		if ("status" in this.body) {
			if (!legacy(version) && this.body.status !== 0 && this.properties !== undefined) {
				throw new Error("only a Normal OBJECT_DATAGRAM may carry properties");
			}
			parts.push(encodeVarint(this.body.status, version));
		} else {
			// Runs to the datagram boundary: written raw, no length prefix.
			parts.push(this.body.payload);
		}

		const out = new Uint8Array(parts.reduce((total, part) => total + part.byteLength, 0));
		let offset = 0;
		for (const part of parts) {
			out.set(part, offset);
			offset += part.byteLength;
		}
		return out;
	}

	/**
	 * Decode one QUIC datagram's bytes. Anything malformed is the peer breaking the protocol,
	 * so every failure is a {@link ProtocolViolation}.
	 */
	static async decode(data: Uint8Array, version: IetfVersion): Promise<ObjectDatagram> {
		try {
			return await new Reader(undefined, data, version).decode((c) => decodeFields(c, version));
		} catch (err: unknown) {
			if (err instanceof ProtocolViolation) throw err;
			throw new ProtocolViolation(`malformed OBJECT_DATAGRAM: ${reason(error(err))}`, { cause: err });
		}
	}
}

/**
 * Encode a datagram the way a publisher sends it: Object 0 of a group that has no other, so it
 * ends its group, stamped in `timescale` units when the subscription declared one.
 */
export async function encodeDatagram(
	datagram: Datagram,
	options: { trackAlias: bigint; publisherPriority: number; timescale?: Timescale },
	version: IetfVersion,
): Promise<Uint8Array> {
	const properties =
		options.timescale !== undefined
			? await encodeObjectExtensions(datagram.timestamp, options.timescale, version)
			: undefined;
	return new ObjectDatagram({
		trackAlias: options.trackAlias,
		groupId: datagram.sequence,
		publisherPriority: options.publisherPriority,
		endOfGroup: true,
		properties: properties?.byteLength ? properties : undefined,
		body: { payload: datagram.payload },
	}).encode(version);
}

function decodeFields(c: Cursor, version: IetfVersion): ObjectDatagram {
	// Full width, so an out-of-range Type is refused rather than overflowing.
	const raw = c.u62();
	const kind = raw <= 0xffn ? Number(raw) : -1;
	if (kind < 0 || !valid(kind, version)) {
		throw new ProtocolViolation(`invalid OBJECT_DATAGRAM type 0x${raw.toString(16)}`);
	}

	const trackAlias = c.u62();
	const groupId = c.u53();
	const objectId = kind & ZERO_OBJECT_ID ? undefined : c.u53();
	const publisherPriority = kind & DEFAULT_PRIORITY ? undefined : c.u8();
	let properties: Uint8Array | undefined;
	if (kind & PROPERTIES) {
		properties = c.read(c.u53());
		if (properties.byteLength === 0) throw new ProtocolViolation("OBJECT_DATAGRAM properties are empty");
	}

	let body: DatagramBody;
	if (kind & STATUS) {
		const status = c.u53();
		if (!legacy(version) && status !== 0 && properties !== undefined) {
			throw new ProtocolViolation("only a Normal OBJECT_DATAGRAM may carry properties");
		}
		if (c.remaining > 0) throw new ProtocolViolation(`OBJECT_DATAGRAM has ${c.remaining} trailing bytes`);
		body = { status };
	} else {
		body = { payload: c.read(c.remaining) };
	}

	return new ObjectDatagram({
		trackAlias,
		groupId,
		objectId,
		publisherPriority,
		endOfGroup: (kind & END_OF_GROUP) !== 0,
		properties,
		body,
	});
}
