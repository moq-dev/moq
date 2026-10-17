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
import { checkTimed, type Reader, type Writer } from "./internal.ts";
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
	// Datagrams waiting in the pump for admission; later ones queue behind them.
	#pending = 0;

	private constructor(inner: NetTrack.Producer, groupKey: TrackKey, datagramKey: TrackKey) {
		this.#inner = inner;
		this.name = inner.name;
		this.#writer = { pump: new Pump(), key: groupKey, closed: false };
		this.#datagramKey = datagramKey;
		const writer = this.#writer;
		inner.info().then(
			(info) => {
				writer.timed = info.timescale != null;
			},
			() => {},
		);
	}

	/** @internal */
	static make(inner: NetTrack.Producer, groupKey: TrackKey, datagramKey: TrackKey): Producer {
		return new Producer(inner, groupKey, datagramKey);
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

	/**
	 * Encrypt `plaintext` at the next sequence and insert the datagram, resolving to that sequence.
	 *
	 * The rules of {@link insertDatagram} apply.
	 */
	async appendDatagram(timestamp: Time.Timestamp | undefined, plaintext: Uint8Array): Promise<number> {
		return this.#insert(undefined, timestamp, plaintext);
	}

	/**
	 * Encrypt `plaintext` at an explicit sequence and insert the datagram.
	 *
	 * Plaintext is capped at `MAX_DATAGRAM_PLAINTEXT` and copied, so the caller may reuse its
	 * buffer. A predictable failure, including a mismatched timestamp, is refused before the
	 * sequence is allocated; once admitted, the identity is spent even if the net write fails.
	 * Until this producer has observed the track's committed timescale, admission waits for it
	 * in the serial pump, so the sequence is allocated there, and later datagrams queue behind it. Rejects with {@link Failure}
	 * `reuse`, `identity`, `exhausted`, or `oversize`, or a net error.
	 */
	async insertDatagram(
		sequence: number,
		timestamp: Time.Timestamp | undefined,
		plaintext: Uint8Array,
	): Promise<void> {
		await this.#insert(sequence, timestamp, plaintext);
	}

	// Admit at `sequence`, or the next one when undefined, then encrypt and write in the pump.
	#insert(
		sequence: number | undefined,
		timestamp: Time.Timestamp | undefined,
		plaintext: Uint8Array,
	): Promise<number> {
		this.#writable();
		const owned = plaintext.slice();
		const writer = this.#writer;
		if (writer.timed !== undefined && this.#pending === 0) {
			const admitted = sequence ?? this.#next;
			const write = this.#admit(admitted, timestamp, owned);
			return writer.pump.run(async () => {
				await write();
				return admitted;
			});
		}
		// Admitting in the pump keeps this ahead of a close or write submitted after it.
		this.#pending++;
		return writer.pump.run(async () => {
			let write: () => Promise<void>;
			let admitted: number;
			try {
				const info = await this.#inner.info();
				writer.timed = info.timescale != null;
				if (writer.aborted) throw writer.aborted;
				admitted = sequence ?? this.#next;
				write = this.#admit(admitted, timestamp, owned);
			} finally {
				this.#pending--;
			}
			await write();
			return admitted;
		});
	}

	// Refuse or spend the datagram's identity now, returning its encrypt-and-write step.
	#admit(sequence: number, timestamp: Time.Timestamp | undefined, owned: Uint8Array<ArrayBuffer>) {
		this.#reserve(sequence);
		checkTimed(this.#writer, timestamp);
		const iv = this.#datagramKey.admitSeal(sequence, 0, owned.byteLength, MAX_DATAGRAM_PAYLOAD);
		this.#next = sequence + 1;
		return async (): Promise<void> => {
			const payload = await this.#datagramKey.encrypt(iv, owned);
			if (this.#writer.aborted) throw this.#writer.aborted;
			this.#inner.insertDatagram(sequence, timestamp, payload);
		};
	}

	/**
	 * Close the track: cleanly after every pending write on it and its groups, or at once with
	 * `abort`, which rejects the pending writes. Idempotent.
	 */
	close(abort?: Error): void {
		if (abort) {
			this.#writer.closed = true;
			this.#writer.aborted ??= abort;
			this.#inner.close(abort);
			return;
		}
		if (this.#writer.closed) return;
		this.#writer.closed = true;
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
 * `authentication`. A bad datagram is an `authentication` {@link Event} until that key is exhausted.
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

	private constructor(inner: NetTrack.Subscriber, groupKey: TrackKey, datagramKey: TrackKey) {
		this.#inner = inner;
		this.name = inner.name;
		this.#reader = { pump: new Pump(), key: groupKey, terminal: new Terminal(inner) };
		this.#datagramKey = datagramKey;
	}

	/** @internal */
	static make(inner: NetTrack.Subscriber, groupKey: TrackKey, datagramKey: TrackKey): Consumer {
		return new Consumer(inner, groupKey, datagramKey);
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
	 * A bad tag and a duplicate are events, not track-ending errors. Rejects with
	 * {@link Failure} `authentication` only if a grouped frame already failed, `oversize` or
	 * `exhausted` from opening, or a net error.
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
		const iv = this.#datagramKey.admitOpen(sequence, 0, payload.byteLength, MAX_DATAGRAM_PAYLOAD);
		let plaintext: Uint8Array;
		try {
			plaintext = await this.#reader.pump.run(() => this.#datagramKey.decrypt(iv, payload));
		} catch (err) {
			// A grouped failure during the open still ends this read.
			this.#reader.terminal.check();
			// A bad datagram is not grouped failure: the track, its demand, and any parked grouped
			// read stay up. `exhausted` is this key's own limit.
			if (err instanceof Failure && err.code === "authentication") return { kind: "authentication", sequence };
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
