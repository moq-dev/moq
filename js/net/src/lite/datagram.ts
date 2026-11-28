/**
 * Wire-level QUIC datagram bodies for moq-lite.
 *
 * @module
 */

import type * as Identity from "../identity.ts";
import * as Path from "../path.ts";
import { encodeVarint, Reader, type Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { decodeNode, decodePin, encodeNode, encodePin } from "./identity.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import { hasDatagrams, hasDatagramTracks, type Version } from "./version.ts";

/**
 * A QUIC datagram body: `subscribe (i) | sequence (i) | timestamp (i) | payload (b)`. Lite-07
 * prefixes datagram-track bodies with type 0x0, its only datagram type.
 *
 * The payload runs to the datagram boundary, so unlike a size-prefixed lite message there is no
 * inner length prefix. Mirrors the Rust `lite::Datagram`; the model counterpart is
 * {@link Datagram} in `../datagram.ts`.
 */
export class Datagram {
	/** The largest encoded datagram body a lite hop carries. */
	static readonly MAX_SIZE = 1200;

	/** Subscribe ID this datagram is delivered on. */
	subscribe: bigint;
	/** The datagram track's sequence, mapped to a group sequence on lite-05/06. */
	sequence: bigint;
	/**
	 * Absolute presentation timestamp in the track's wire timescale. A scale-less track's is the
	 * bare value its producer gave, which is the datagram's sequence.
	 */
	timestamp: bigint;
	/** The frame payload, delimited by the datagram boundary. */
	payload: Uint8Array;

	constructor(subscribe: bigint, sequence: bigint, timestamp: bigint, payload: Uint8Array) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.timestamp = timestamp;
		this.payload = payload;
	}

	/** Encode the body to a single `Uint8Array` (no length prefix; the datagram boundary delimits it). */
	encode(version: Version): Uint8Array {
		if (!hasDatagrams(version)) throw new Error("datagrams require lite-05 or newer");
		const subscribe = encodeVarint(this.subscribe, version);
		const sequence = encodeVarint(this.sequence, version);
		const timestamp = encodeVarint(this.timestamp, version);
		const type = hasDatagramTracks(version) ? encodeVarint(0x0, version) : new Uint8Array();

		const out = new Uint8Array(
			type.byteLength +
				subscribe.byteLength +
				sequence.byteLength +
				timestamp.byteLength +
				this.payload.byteLength,
		);
		let offset = 0;
		out.set(type, offset);
		offset += type.byteLength;
		out.set(subscribe, offset);
		offset += subscribe.byteLength;
		out.set(sequence, offset);
		offset += sequence.byteLength;
		out.set(timestamp, offset);
		offset += timestamp.byteLength;
		out.set(this.payload, offset);
		return out;
	}

	/** Decode a datagram body. An unknown lite-07 type throws, so the caller discards it. */
	static async decode(data: Uint8Array, version: Version): Promise<Datagram> {
		if (!hasDatagrams(version)) throw new Error("datagrams require lite-05 or newer");
		const r = new Reader(undefined, data, version);
		if (hasDatagramTracks(version)) {
			const type = await r.u62();
			if (type !== 0n) throw new Error(`unknown datagram type: ${type}`);
		}
		const subscribe = await r.u62();
		const sequence = await r.u62();
		const timestamp = await r.u62();
		const payload = await r.readAll();
		return new Datagram(subscribe, sequence, timestamp, payload);
	}
}

function guard(version: Version): void {
	if (!hasDatagramTracks(version)) throw new Error("datagram tracks require lite-07 or newer");
}

/** The subscriber's first message on a Datagrams Stream (0x9), live from now. */
export class Datagrams {
	/** The shared SUBSCRIBE/DATAGRAMS identifier space. */
	id: bigint;
	/** The requested broadcast path. */
	broadcast: Path.Valid;
	/** The publisher identity the subscriber expects. */
	identity?: Identity.Valid;
	/** The datagram track's name. */
	track: string;
	/** The subscriber priority, higher first. */
	priority: number;

	constructor(properties: {
		id: bigint;
		broadcast: Path.Valid;
		identity?: Identity.Valid;
		track: string;
		priority: number;
	}) {
		this.identity = properties.identity;
		this.id = properties.id;
		this.broadcast = properties.broadcast;
		this.track = properties.track;
		this.priority = properties.priority;
	}

	/** Encode the size-prefixed request. */
	async encode(writer: Writer, version: Version): Promise<void> {
		guard(version);
		await Message.encode(writer, async (w) => {
			await w.u62(this.id);
			await w.string(Path.encode(this.broadcast));
			await encodePin(w, version, this.identity);
			await encodeNode(w, version);
			await w.string(this.track);
			await w.u8(toWire(this.priority));
		});
	}

	/** Decode the size-prefixed request. */
	static async decode(reader: Reader, version: Version): Promise<Datagrams> {
		guard(version);
		return Message.decode(reader, async (r) => {
			const id = await r.u62();
			const broadcast = Path.decode(await r.string());
			const identity = await decodePin(r, version);
			await decodeNode(r, version);
			return new Datagrams({
				id,
				broadcast,
				identity,
				track: await r.string(),
				priority: fromWire(await r.u8()),
			});
		});
	}
}

/** A subscriber priority update on a Datagrams Stream. */
export class DatagramsUpdate {
	/** The subscriber's replacement priority. */
	priority: number;
	constructor(priority: number) {
		this.priority = priority;
	}
	/** Encode the size-prefixed update. */
	async encode(writer: Writer, version: Version): Promise<void> {
		guard(version);
		await Message.encode(writer, async (w) => {
			await w.u8(toWire(this.priority));
		});
	}
	/** Decode an update, or undefined at the subscriber's FIN. */
	static async decodeMaybe(reader: Reader, version: Version): Promise<DatagramsUpdate | undefined> {
		guard(version);
		return Message.decodeMaybe(reader, async (r) => new DatagramsUpdate(fromWire(await r.u8())));
	}
}

/** The publisher's sole answer on a Datagrams Stream, fixing timestamp framing. */
export class DatagramsInfo {
	/** Publisher priority, breaking ties at one subscriber priority. */
	priority: number;
	/** Timestamp units per second, or undefined for an untimed track. */
	timescale?: Timescale;
	constructor(properties: { priority: number; timescale?: Timescale | null }) {
		this.priority = properties.priority;
		this.timescale = properties.timescale ?? undefined;
	}
	/** Encode the size-prefixed answer. */
	async encode(writer: Writer, version: Version): Promise<void> {
		guard(version);
		await Message.encode(writer, async (w) => {
			await w.u8(toWire(this.priority));
			await w.u53(this.timescale ?? 0);
		});
	}
	/** Decode the publisher properties. */
	static async decode(reader: Reader, version: Version): Promise<DatagramsInfo> {
		guard(version);
		return Message.decode(reader, async (r) => {
			const priority = fromWire(await r.u8());
			const scale = await r.u53();
			return new DatagramsInfo({ priority, timescale: scale === 0 ? undefined : Timescale(scale) });
		});
	}
}
