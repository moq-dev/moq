/**
 * Grouped-frame and datagram writers and readers for one physical track.
 *
 * @module
 */
import { Group as NetGroup, type Track as NetTrack, type Time } from "@moq/net";
import type { Event } from "./datagram.ts";
import { checkU53 } from "./derive.ts";
import { Failure } from "./failure.ts";
import * as Group from "./group.ts";
import { checkTimestamp, type Keys, type Reader, type Writer } from "./internal.ts";
import type { TrackKey } from "./key.ts";
import { MAX_DATAGRAM_PAYLOAD } from "./limits.ts";
import { Pump } from "./pump.ts";
import { Terminal } from "./terminal.ts";
import { DatagramWindow } from "./window.ts";

/**
 * Exclusive producer for one physical track.
 *
 * Owns the grouped-frame and datagram keys and the shared sequence namespace. Sequences are
 * allocated monotonically; reuse and exhaustion are refused before encryption. Every write
 * on the track and its groups runs through one serial pump, so net writes land in call order.
 */
export class Producer {
	/** The physical track name. */
	readonly name: string;

	#inner: NetTrack.Producer;
	#writer: Writer;
	#datagramKey: TrackKey;
	#next = 0;

	private constructor(inner: NetTrack.Producer, timescale: Time.Timescale | undefined, keys: Keys) {
		this.#inner = inner;
		this.name = inner.name;
		this.#writer = { pump: new Pump(), key: keys.group, timescale, closed: false };
		this.#datagramKey = keys.datagram;
	}

	/** @internal */
	static make(inner: NetTrack.Producer, timescale: Time.Timescale | undefined, keys: Keys): Producer {
		return new Producer(inner, timescale, keys);
	}

	/** Allocate the next sequence and start a group there; throws {@link Failure} `identity` past `2^53-1`. */
	appendGroup(): Group.Producer {
		return this.createGroup(this.#next);
	}

	/**
	 * Start a group at an explicit sequence, which must be at or above the next unallocated one.
	 *
	 * Throws {@link Failure} `reuse` if `sequence` was already allocated, `identity` if it
	 * exceeds `2^53-1`, or a net error.
	 */
	createGroup(sequence: number): Group.Producer {
		this.#writable();
		this.#reserve(sequence);
		this.#next = sequence + 1;
		const inner = new NetGroup.Producer(sequence);
		this.#inner.writeGroup(inner);
		return Group.Producer.make(inner, this.#writer);
	}

	/** Encrypt `plaintext` at the next sequence and insert the datagram, resolving to that sequence. */
	async appendDatagram(timestamp: Time.Timestamp | undefined, plaintext: Uint8Array): Promise<number> {
		const sequence = this.#next;
		await this.insertDatagram(sequence, timestamp, plaintext);
		return sequence;
	}

	/**
	 * Encrypt `plaintext` at an explicit sequence and insert the datagram.
	 *
	 * Plaintext is capped at `MAX_DATAGRAM_PLAINTEXT` and copied, so the caller may reuse its
	 * buffer. A predictable failure, including a mismatched timestamp, is refused before the
	 * sequence is allocated; once admitted, the identity is spent even if the net write fails.
	 * Rejects with {@link Failure} `reuse`, `identity`, `exhausted`, or `oversize`, or a net error.
	 */
	async insertDatagram(
		sequence: number,
		timestamp: Time.Timestamp | undefined,
		plaintext: Uint8Array,
	): Promise<void> {
		this.#writable();
		this.#reserve(sequence);
		checkTimestamp(this.#writer, timestamp);
		const iv = this.#datagramKey.admitSeal(sequence, 0, plaintext.byteLength, MAX_DATAGRAM_PAYLOAD);
		this.#next = sequence + 1;
		const owned = plaintext.slice();
		return this.#writer.pump.run(async () => {
			if (this.#writer.aborted) throw this.#writer.aborted;
			const payload = await this.#datagramKey.encrypt(iv, owned);
			if (this.#writer.aborted) throw this.#writer.aborted;
			this.#inner.insertDatagram(sequence, timestamp, payload);
		});
	}

	/**
	 * Close the track: cleanly after every pending write on it and its groups, or at once with
	 * `abort`, which rejects the pending writes. Idempotent.
	 */
	close(abort?: Error): void {
		// The first close decides the outcome: a later abort cannot cancel a promised drain.
		if (this.#writer.closed) return;
		this.#writer.closed = true;
		if (abort) {
			this.#writer.aborted = abort;
			this.#inner.close(abort);
			return;
		}
		void this.#writer.pump.run(() => {
			if (!this.#writer.aborted) this.#inner.close();
		});
	}

	#writable(): void {
		if (this.#writer.closed) throw this.#writer.aborted ?? new Error("track is closed");
	}

	// Refuse a sequence this track already allocated or cannot represent.
	#reserve(sequence: number): void {
		checkU53(sequence);
		if (sequence < this.#next) throw new Failure("reuse");
	}
}

/**
 * Subscriber for one physical track.
 *
 * Grouped authentication failure ends the track and rejects pending reads with {@link Failure}
 * `authentication`. A datagram that fails to open is dropped as an `identity`, `oversize`, or
 * `authentication` {@link Event} until that key is exhausted.
 */
export class Consumer {
	/** The physical track name. */
	readonly name: string;

	#inner: NetTrack.Subscriber;
	#reader: Reader;
	#datagramKey: TrackKey;
	#window = new DatagramWindow();
	// Datagram reads run one at a time, so the window and key see them in arrival order.
	#datagrams: Promise<unknown> = Promise.resolve();

	private constructor(inner: NetTrack.Subscriber, keys: Keys) {
		this.#inner = inner;
		this.name = inner.name;
		this.#reader = { pump: new Pump(), key: keys.group, terminal: new Terminal(inner) };
		this.#datagramKey = keys.datagram;
	}

	/** @internal */
	static make(inner: NetTrack.Subscriber, keys: Keys): Consumer {
		return new Consumer(inner, keys);
	}

	/**
	 * Receive the next protected group in arrival order, or `undefined` once the track ends.
	 *
	 * Rejects with {@link Failure} `authentication` once a grouped frame has failed to open, or a net error.
	 */
	async recvGroup(): Promise<Group.Consumer | undefined> {
		this.#reader.terminal.check();
		let group: NetGroup.Consumer | undefined;
		try {
			group = await this.#inner.recvGroup();
		} catch (err) {
			this.#reader.terminal.check();
			throw err;
		}
		if (this.#reader.terminal.failed) {
			group?.close();
			throw this.#reader.terminal.failed;
		}
		return group && Group.Consumer.make(group, this.#reader);
	}

	/**
	 * Receive the next datagram event, or `undefined` once the track ends.
	 *
	 * A datagram that fails to open (a sequence out of range, a bad size, or a forged tag) and a
	 * duplicate are events, not track-ending errors. Rejects with {@link Failure}
	 * `authentication` only if a grouped frame already failed, `exhausted` from opening, or a
	 * net error.
	 */
	recvDatagram(): Promise<Event | undefined> {
		const result = this.#datagrams.then(() => this.#recvDatagram());
		this.#datagrams = result.catch(() => {});
		return result;
	}

	async #recvDatagram(): Promise<Event | undefined> {
		this.#reader.terminal.check();
		let datagram: NetTrack.Datagram | undefined;
		try {
			datagram = await this.#inner.recvDatagram();
		} catch (err) {
			this.#reader.terminal.check();
			throw err;
		}
		this.#reader.terminal.check();
		if (!datagram) return undefined;

		const { sequence, timestamp, payload } = datagram;
		if (this.#window.isDuplicate(sequence)) return { kind: "duplicate", sequence };
		let plaintext: Uint8Array;
		try {
			const iv = this.#datagramKey.admitOpen(sequence, 0, payload.byteLength, MAX_DATAGRAM_PAYLOAD);
			plaintext = await this.#reader.pump.run(() => {
				this.#reader.terminal.check();
				return this.#datagramKey.decrypt(iv, payload);
			});
		} catch (err) {
			// A grouped failure during the open still ends this read.
			this.#reader.terminal.check();
			// A bad datagram is not grouped failure: the track, its demand, and any parked grouped
			// read stay up. `exhausted` is this key's own limit.
			if (!(err instanceof Failure)) throw err;
			const kind = err.code;
			if (kind === "authentication" || kind === "identity" || kind === "oversize") return { kind, sequence };
			throw err;
		}
		this.#reader.terminal.check();
		this.#window.mark(sequence);
		return { kind: "datagram", datagram: { sequence, timestamp, plaintext } };
	}

	/**
	 * Unsubscribe, releasing track demand. As in `@moq/net`, groups already received end
	 * after the frames they hold. Idempotent.
	 */
	close(): void {
		this.#reader.terminal.unsubscribe();
	}
}
