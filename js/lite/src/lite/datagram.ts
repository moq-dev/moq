// Wire-level datagram messages for moq-lite-04-datagrams.
//
// Contains both the per-datagram QUIC datagram body codec (`Datagram`) and
// the control-stream messages (`Datagrams`, `DatagramsOk`, `DatagramsUpdate`).

import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

function checkLite04Datagrams(version: Version) {
	if (version !== Version.DRAFT_04_DATAGRAMS) {
		throw new Error(`datagrams not supported on version: 0x${version.toString(16)}`);
	}
}

/**
 * A single QUIC datagram body: `subscribe_id (i) | sequence (i) | payload (b)`.
 *
 * The QUIC datagram boundary delimits the payload — there is no inner length
 * prefix. moq-lite-04-datagrams ignores the sequence number for delivery semantics; the
 * field is preserved so the same encoding can be reused by an `moq-transport`
 * adapter (deferred).
 */
export class Datagram {
	subscribe: bigint;
	sequence: number;
	payload: Uint8Array;

	constructor(subscribe: bigint, sequence: number, payload: Uint8Array) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.payload = payload;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		checkLite04Datagrams(version);
		await w.u62(this.subscribe);
		await w.u53(this.sequence);
		if (this.payload.byteLength > 0) {
			await w.write(this.payload);
		}
	}

	static async decode(r: Reader, version: Version): Promise<Datagram> {
		checkLite04Datagrams(version);
		const subscribe = await r.u62();
		const sequence = await r.u53();
		const payload = await r.readAll();
		return new Datagram(subscribe, sequence, payload);
	}
}

/**
 * Sent by the subscriber to request datagram delivery for a track.
 */
export class Datagrams {
	id: bigint;
	broadcast: Path.Valid;
	track: string;
	/** Tolerated cache age in milliseconds. `0` is strict. */
	maxLatency: number;

	constructor(props: { id: bigint; broadcast: Path.Valid; track: string; maxLatency: number }) {
		this.id = props.id;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.maxLatency = props.maxLatency;
	}

	async #encode(w: Writer, version: Version): Promise<void> {
		checkLite04Datagrams(version);
		await w.u62(this.id);
		await w.string(this.broadcast);
		await w.string(this.track);
		await w.u53(this.maxLatency);
	}

	static async #decode(r: Reader, version: Version): Promise<Datagrams> {
		checkLite04Datagrams(version);
		const id = await r.u62();
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		const maxLatency = await r.u53();
		return new Datagrams({ id, broadcast, track, maxLatency });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Datagrams> {
		return Message.decode(r, (r) => Datagrams.#decode(r, version));
	}
}

/**
 * Publisher's acknowledgement of a Datagrams subscription.
 */
export class DatagramsOk {
	maxLatency: number;

	constructor(maxLatency: number) {
		this.maxLatency = maxLatency;
	}

	async #encode(w: Writer, version: Version): Promise<void> {
		checkLite04Datagrams(version);
		await w.u53(this.maxLatency);
	}

	static async #decode(r: Reader, version: Version): Promise<DatagramsOk> {
		checkLite04Datagrams(version);
		return new DatagramsOk(await r.u53());
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<DatagramsOk> {
		return Message.decode(r, (r) => DatagramsOk.#decode(r, version));
	}
}

/**
 * Subscriber updating an existing Datagrams subscription.
 */
export class DatagramsUpdate {
	maxLatency: number;

	constructor(maxLatency: number) {
		this.maxLatency = maxLatency;
	}

	async #encode(w: Writer, version: Version): Promise<void> {
		checkLite04Datagrams(version);
		await w.u53(this.maxLatency);
	}

	static async #decode(r: Reader, version: Version): Promise<DatagramsUpdate> {
		checkLite04Datagrams(version);
		return new DatagramsUpdate(await r.u53());
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<DatagramsUpdate> {
		return Message.decode(r, (r) => DatagramsUpdate.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<DatagramsUpdate | undefined> {
		return Message.decodeMaybe(r, (r) => DatagramsUpdate.#decode(r, version));
	}
}
