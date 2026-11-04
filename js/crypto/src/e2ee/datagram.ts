/**
 * Decrypted datagrams and the events a protected datagram read yields.
 *
 * @module
 */
import { Datagram as NetDatagram, type Time } from "@moq/net";
import { Pump } from "../pump.ts";
import { checkGroup } from "./derive.ts";
import { Failure } from "./failure.ts";
import type { TrackKey } from "./key.ts";
import { MAX_DATAGRAM_PAYLOAD } from "./limits.ts";
import { DatagramWindow } from "./window.ts";

/** A decrypted datagram. */
export interface Datagram {
	/** The datagram track's own sequence. */
	sequence: bigint;
	/** Presentation timestamp, or `undefined` when untimed. Not authenticated. */
	timestamp?: Time.Timestamp;
	/** Decrypted application bytes. */
	plaintext: Uint8Array;
}

/**
 * Outcome of reading one protected datagram.
 *
 * - `datagram`: authenticated plaintext.
 * - `authentication`: the datagram failed to authenticate; dropped, track continues.
 * - `duplicate`: the sequence was already opened inside the retained window; dropped, track continues.
 * - `identity`: the sequence is above `2^53-1`; dropped, track continues.
 * - `oversize`: the payload is shorter than the tag or longer than the datagram cap; dropped, track continues.
 */
export type Event =
	| { kind: "datagram"; datagram: Datagram }
	| { kind: "authentication"; sequence: bigint }
	| { kind: "duplicate"; sequence: bigint }
	| { kind: "identity"; sequence: bigint }
	| { kind: "oversize"; sequence: bigint };

/** Encrypts a datagram track under its name's datagram key and independent sequence space. */
export class Producer {
	/** The physical track name. */
	readonly name: string;
	#inner: NetDatagram.Producer;
	#info: NetDatagram.Info;
	#key: TrackKey;
	#pump = new Pump();
	#next = 0n;
	#finished = false;
	#aborted?: Error;

	private constructor(inner: NetDatagram.Producer, info: NetDatagram.Info, key: TrackKey) {
		this.#inner = inner;
		this.#info = info;
		this.#key = key;
		this.name = inner.name;
	}
	/** @internal */
	static make(inner: NetDatagram.Producer, info: NetDatagram.Info, key: TrackKey): Producer {
		return new Producer(inner, info, key);
	}
	/** The highest priority among the datagram track's subscribers. */
	get priority(): NetDatagram.Producer["priority"] {
		return this.#inner.priority;
	}
	/** The datagram track's terminal result. */
	get closed(): NetDatagram.Producer["closed"] {
		return this.#inner.closed;
	}
	/** Encrypt a payload at the next datagram sequence, returning that sequence. */
	async append(timestamp: Time.Timestamp | undefined, plaintext: Uint8Array): Promise<bigint> {
		const sequence = this.#next;
		await this.insert(sequence, timestamp, plaintext);
		return sequence;
	}
	/** Encrypt at an explicit unused sequence, refusing predictable failures before allocating it. */
	async insert(sequence: bigint, timestamp: Time.Timestamp | undefined, plaintext: Uint8Array): Promise<void> {
		if (this.#finished || this.#aborted) throw this.#aborted ?? new Error("datagram track is closed");
		checkGroup(sequence);
		if (sequence < this.#next) throw new Failure("reuse");
		const at = NetDatagram.rescale(this.#info, timestamp);
		const iv = this.#key.admitSeal(sequence, 0, plaintext.byteLength, MAX_DATAGRAM_PAYLOAD);
		this.#next = sequence + 1n;
		const owned = plaintext.slice();
		return this.#pump.run(async () => {
			if (this.#aborted) throw this.#aborted;
			const payload = await this.#key.encrypt(iv, owned);
			if (this.#aborted) throw this.#aborted;
			this.#inner.insert({ sequence, timestamp: at, payload });
		});
	}
	/** Finish after every queued write, refusing new writes immediately. */
	finish(): void {
		if (this.#finished || this.#aborted) return;
		this.#finished = true;
		void this.#pump.run(() => {
			if (!this.#aborted) this.#inner.finish();
		});
	}
	/** Abort immediately, rejecting queued writes. */
	abort(error: Error): void {
		if (this.#aborted) return;
		this.#aborted = error;
		this.#inner.abort(error);
	}
}

/** Decrypts a datagram track, reporting malformed and replayed datagrams as events. */
export class Consumer {
	/** The physical track name. */
	readonly name: string;
	#inner: NetDatagram.Consumer;
	#key: TrackKey;
	#window = new DatagramWindow();
	#pump = new Pump();
	#reads: Promise<unknown> = Promise.resolve();
	#closed = false;

	private constructor(inner: NetDatagram.Consumer, key: TrackKey) {
		this.#inner = inner;
		this.#key = key;
		this.name = inner.name;
	}
	/** @internal */
	static make(inner: NetDatagram.Consumer, key: TrackKey): Consumer {
		return new Consumer(inner, key);
	}
	/** This consumer's subscriber priority. */
	get priority(): number {
		return this.#inner.priority;
	}
	/** Change this consumer's priority, higher first. */
	setPriority(priority: number): void {
		this.#inner.setPriority(priority);
	}
	/** Read the next datagram event, or undefined after a clean finish. */
	recv(): Promise<Event | undefined> {
		const result = this.#reads.then(() => this.#recv());
		this.#reads = result.catch(() => {});
		return result;
	}
	async #recv(): Promise<Event | undefined> {
		if (this.#closed) return;
		const datagram = await this.#inner.recv();
		if (!datagram || this.#closed) return;
		const { sequence, timestamp, payload } = datagram;
		if (this.#window.isDuplicate(sequence)) return { kind: "duplicate", sequence };
		let plaintext: Uint8Array;
		try {
			const iv = this.#key.admitOpen(sequence, 0, payload.byteLength, MAX_DATAGRAM_PAYLOAD);
			plaintext = await this.#pump.run(() => this.#key.decrypt(iv, payload));
		} catch (err) {
			if (this.#closed) return;
			if (!(err instanceof Failure)) throw err;
			const kind = err.code;
			if (kind === "authentication" || kind === "identity" || kind === "oversize") return { kind, sequence };
			throw err;
		}
		if (this.#closed) return;
		this.#window.mark(sequence);
		return { kind: "datagram", datagram: { sequence, timestamp, plaintext } };
	}
	/** Release this consumer's demand and wake its pending reads. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#inner.close();
	}
}
