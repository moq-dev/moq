import { Group, Time, type Track } from "@moq/net";
import type { Getter } from "@moq/signals";
import { Signal } from "@moq/signals";
import { DOMAIN_DATAGRAM, TAG_LEN } from "./constants.ts";
import type { Credential } from "./credential.ts";
import { datagramPayloadLimit, insertDatagram } from "./datagram.ts";
import { Failure } from "./error.ts";
import { GroupConsumer, GroupProducer, type SealedFrame } from "./group.ts";
import { Pump } from "./pump.ts";
import { DatagramWindow, GroupWindow } from "./window.ts";

/** Options for a protected {@link Producer}. */
export interface ProducerOptions {
	/** Underlying track; its name must already be the opaque physical name. */
	track: Track.Producer;
	/** Broadcast credential. */
	credential: Credential;
	/**
	 * If set, derived and checked against {@link track}'s name so a semantic name
	 * cannot leak onto the wire.
	 */
	semanticName?: string;
	/** Pump in-flight bound. */
	pumpDepth?: number;
	/** Pump waiter bound. */
	pumpQueue?: number;
	/**
	 * Subscribe ID used to size the datagram payload budget. Defaults to 1, matching
	 * the shared vectors; pass the real ID when it is larger so the header cannot
	 * push a body over 1200 bytes.
	 */
	subscribeId?: bigint | number;
	/** Timescale used to size datagram headers. Defaults to milliseconds. */
	timescale?: Time.Timescale;
}

/** Options for a protected {@link Consumer}. */
export interface ConsumerOptions {
	/** Underlying subscription. Groups are read in sequence order. */
	track: Track.Subscriber;
	/** Broadcast credential. */
	credential: Credential;
	/** If set, derived and checked against the track name. */
	semanticName?: string;
	/** Pump in-flight bound. */
	pumpDepth?: number;
	/** Pump waiter bound. */
	pumpQueue?: number;
	/** Subscribe ID used to size the datagram payload budget. Defaults to 1. */
	subscribeId?: bigint | number;
	/** Datagram duplicate-window size. Defaults to 1024. */
	datagramWindow?: number;
	/** Timescale used to size datagram headers. Defaults to milliseconds. */
	timescale?: Time.Timescale;
}

/** A dropped datagram: authentication failure or operational duplicate. */
export interface DatagramEvent {
	/** Monotonic id so successive events always notify. */
	id: number;
	/** Why the datagram was dropped. */
	code: "authentication" | "duplicate" | "oversize" | "identity" | "exhausted";
	/** Datagram sequence. */
	sequence: number;
}

function wireTimestamp(timestamp: Time.Timestamp, timescale: Time.Timescale): number {
	return Math.round(timestamp.as(timescale));
}

/**
 * Protects every group and datagram written to a track.
 *
 * Owns frame ordinals, datagram sequences, and ciphertext for retransmission.
 * The underlying track's name is the physical name; semantic names stay inside
 * ciphertext (catalogs) or out of band.
 */
export class Producer {
	/** Physical track name. */
	readonly name: string;

	#track: Track.Producer;
	#credential: Credential;
	#pump: Pump;
	#subscribeId: bigint | number;
	#timescale: Time.Timescale;
	#next = 0;

	private constructor(options: ProducerOptions) {
		this.#track = options.track;
		this.#credential = options.credential;
		this.name = options.track.name;
		this.#pump = new Pump({ depth: options.pumpDepth, queue: options.pumpQueue });
		this.#subscribeId = options.subscribeId ?? 1;
		this.#timescale = options.timescale ?? Time.Timescale.MILLI;
		this.#credential.claimPublisher(this.name);
	}

	/** Derive optional name checks, then wrap `track`. */
	static async create(options: ProducerOptions): Promise<Producer> {
		if (options.semanticName !== undefined) {
			const physical = await options.credential.opaqueName(options.semanticName);
			if (physical !== options.track.name) {
				throw new Failure("identity", "track name is not the opaque name for this credential");
			}
		}
		return new Producer(options);
	}

	/** Append a new group with the next sequence number. */
	appendGroup(): GroupProducer {
		const sequence = this.#next++;
		const inner = new Group.Producer(sequence);
		this.#track.writeGroup(inner);
		const group = new GroupProducer(inner, this.#credential, this.name, this.#pump);
		return group;
	}

	/**
	 * Insert `group` by sequence. A live duplicate throws; an aborted group may be
	 * replaced only by {@link retransmit} of its ciphertext.
	 */
	writeGroup(group: GroupProducer): void {
		if (group.sequence >= this.#next) this.#next = group.sequence + 1;
		this.#track.writeGroup(group.inner());
	}

	/**
	 * Re-publish stored ciphertext for `group` at the same identity. Encrypting
	 * different bytes at that identity is `reuse`; this path never encrypts.
	 */
	retransmit(group: GroupProducer): void {
		const frames = group.sealed();
		if (frames.length === 0) throw new Error("group has no ciphertext to retransmit");
		const inner = new Group.Producer(group.sequence);
		for (const frame of frames) inner.writeFrame(frame);
		if (group.sequence >= this.#next) this.#next = group.sequence + 1;
		this.#track.writeGroup(inner);
	}

	/** Encrypt `payload` as the next datagram and insert it. */
	async appendDatagram(timestamp: Time.Timestamp, payload: Uint8Array): Promise<number> {
		const sequence = this.#next++;
		await this.#insert(sequence, timestamp, payload);
		return sequence;
	}

	/** Encrypt `payload` at an explicit datagram sequence and insert it. */
	async insertDatagram(sequence: number, timestamp: Time.Timestamp, payload: Uint8Array): Promise<void> {
		if (sequence >= this.#next) this.#next = sequence + 1;
		await this.#insert(sequence, timestamp, payload);
	}

	/**
	 * Insert stored ciphertext at `sequence` without encrypting. Missing ciphertext
	 * is a usage error: encrypt once with {@link insertDatagram}, then retransmit.
	 */
	retransmitDatagram(sequence: number, timestamp: Time.Timestamp): void {
		const payload = this.#credential.ciphertext(this.name, DOMAIN_DATAGRAM, sequence, 0);
		if (!payload) throw new Error("datagram has no ciphertext to retransmit");
		insertDatagram(this.#track, sequence, timestamp, payload);
	}

	/** Append a single-frame group. */
	async writeFrame(frame: Group.Frame): Promise<void> {
		const group = this.appendGroup();
		try {
			await group.writeFrame(frame);
			group.close();
		} catch (error) {
			group.close(error instanceof Error ? error : new Error(String(error)));
			throw error;
		}
	}

	/** Drain in-flight AEAD then close the track. */
	async finish(): Promise<void> {
		await this.#pump.drain();
		this.#track.close();
	}

	/** Cancel in-flight work and close the track. */
	close(abort?: Error): void {
		this.#pump.close(abort ?? new Error("closed"));
		this.#track.close(abort);
	}

	async #insert(sequence: number, timestamp: Time.Timestamp, payload: Uint8Array): Promise<void> {
		const header = {
			subscribe: this.#subscribeId,
			sequence,
			timestamp: wireTimestamp(timestamp, this.#timescale),
		};
		const payloadLimit = datagramPayloadLimit(header);
		if (payload.byteLength + TAG_LEN > payloadLimit) throw new Failure("oversize");
		const ciphertext = await this.#pump.submit(() =>
			this.#credential.seal({
				physicalName: this.name,
				domain: DOMAIN_DATAGRAM,
				group: sequence,
				frame: 0,
				plaintext: payload,
				payloadLimit,
			}),
		);
		insertDatagram(this.#track, sequence, timestamp, ciphertext);
	}
}

/**
 * Opens every group and datagram read from a track.
 *
 * Grouped authentication failure ends the track. A bad datagram is dropped and
 * surfaced on {@link events}; the track continues.
 */
export class Consumer {
	/** Physical track name. */
	readonly name: string;

	#ordered: Track.Ordered;
	#credential: Credential;
	#pump: Pump;
	#subscribeId: bigint | number;
	#timescale: Time.Timescale;
	#groups = new GroupWindow();
	#datagrams: DatagramWindow;
	#events = new Signal<DatagramEvent | undefined>(undefined);
	#eventId = 0;
	#closed?: Failure;

	private constructor(options: ConsumerOptions, ordered: Track.Ordered) {
		this.#ordered = ordered;
		this.#credential = options.credential;
		this.name = ordered.name;
		this.#pump = new Pump({ depth: options.pumpDepth, queue: options.pumpQueue });
		this.#subscribeId = options.subscribeId ?? 1;
		this.#timescale = options.timescale ?? Time.Timescale.MILLI;
		this.#datagrams = new DatagramWindow(options.datagramWindow);
	}

	/** Derive optional name checks, then wrap `track`. */
	static async create(options: ConsumerOptions): Promise<Consumer> {
		if (options.semanticName !== undefined) {
			const physical = await options.credential.opaqueName(options.semanticName);
			if (physical !== options.track.name) {
				throw new Failure("identity", "track name is not the opaque name for this credential");
			}
		}
		return new Consumer(options, options.track.ordered());
	}

	/** Latest dropped-datagram event, if any. */
	get events(): Getter<DatagramEvent | undefined> {
		return this.#events;
	}

	/** Next group in sequence order, decrypting its frames. */
	async nextGroup(): Promise<GroupConsumer | undefined> {
		this.#throwIfClosed();
		try {
			const inner = await this.#ordered.nextGroup();
			if (!inner) return undefined;
			return new GroupConsumer(inner, this.#credential, this.name, this.#pump, this.#groups, (error) =>
				this.#fail(error),
			);
		} catch (error) {
			this.#fail(error);
			throw error;
		}
	}

	/**
	 * Next authentic datagram, skipping duplicates and authentication failures.
	 * Those are emitted on {@link events}; the track stays open.
	 */
	async recvDatagram(): Promise<Track.Datagram | undefined> {
		this.#throwIfClosed();
		for (;;) {
			const datagram = await this.#ordered.recvDatagram();
			if (!datagram) return undefined;
			try {
				this.#datagrams.claim(datagram.sequence);
			} catch (error) {
				if (error instanceof Failure && error.code === "duplicate") {
					this.#emit("duplicate", datagram.sequence);
					continue;
				}
				throw error;
			}
			let payloadLimit: number;
			try {
				payloadLimit = datagramPayloadLimit({
					subscribe: this.#subscribeId,
					sequence: datagram.sequence,
					timestamp: wireTimestamp(datagram.timestamp, this.#timescale),
				});
			} catch (error) {
				if (error instanceof Failure && error.code === "oversize") {
					this.#emit("oversize", datagram.sequence);
					continue;
				}
				throw error;
			}
			// Authentication (and other droppable failures) must not kill the pump: a bad
			// datagram is skipped and the track continues.
			const opened = await this.#pump.submit(async () => {
				try {
					return await this.#credential.open({
						physicalName: this.name,
						domain: DOMAIN_DATAGRAM,
						group: datagram.sequence,
						frame: 0,
						payload: datagram.payload,
						payloadLimit,
					});
				} catch (error) {
					if (
						error instanceof Failure &&
						(error.code === "authentication" || error.code === "oversize" || error.code === "identity")
					) {
						return error;
					}
					throw error;
				}
			});
			if (opened instanceof Failure) {
				this.#emit(opened.code as DatagramEvent["code"], datagram.sequence);
				continue;
			}
			return { sequence: datagram.sequence, timestamp: datagram.timestamp, payload: opened };
		}
	}

	/** Close the subscription. */
	close(abort?: Error): void {
		this.#pump.close(abort ?? new Error("closed"));
		this.#ordered.close(abort);
	}

	#emit(code: DatagramEvent["code"], sequence: number): void {
		this.#events.set({ id: ++this.#eventId, code, sequence });
	}

	#throwIfClosed(): void {
		if (this.#closed) throw this.#closed;
	}

	#fail(error: unknown): void {
		if (error instanceof Failure && error.code === "authentication") {
			this.#closed = error;
			this.#pump.close(error);
			this.#ordered.close(error);
		}
	}
}

export type { SealedFrame };
