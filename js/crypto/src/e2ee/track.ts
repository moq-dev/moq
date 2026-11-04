/**
 * Grouped-frame writers and readers for one physical track.
 *
 * @module
 */
import type { Group as NetGroup, Track as NetTrack } from "@moq/net";
import { Pump } from "../pump.ts";
import { checkGroup } from "./derive.ts";
import { Failure } from "./failure.ts";
import * as Group from "./group.ts";
import type { Reader, Writer } from "./internal.ts";
import type { TrackKey } from "./key.ts";
import { Terminal } from "./terminal.ts";

/**
 * Exclusive producer for one physical track.
 *
 * Owns the grouped-frame key and sequence namespace. Sequences are
 * allocated monotonically; reuse and exhaustion are refused before encryption. Every write
 * on the track and its groups runs through one serial pump, so net writes land in call order.
 */
export class Producer {
	/** The physical track name. */
	readonly name: string;

	#inner: NetTrack.Producer;
	#writer: Writer;
	#next = 0n;
	#finished = false;

	private constructor(inner: NetTrack.Producer, info: NetTrack.Info, key: TrackKey) {
		this.#inner = inner;
		this.name = inner.name;
		this.#writer = { pump: new Pump(), key, track: info, closed: false };
	}

	/** @internal */
	static make(inner: NetTrack.Producer, info: NetTrack.Info, key: TrackKey): Producer {
		return new Producer(inner, info, key);
	}

	/** The track's final outcome, once its declared end is reached or it aborts. */
	get closed(): NetTrack.Producer["closed"] {
		return this.#inner.closed;
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
	createGroup(sequence: bigint): Group.Producer {
		this.#writable();
		this.#reserve(sequence);
		this.#next = sequence + 1n;
		const inner = this.#inner.createGroup(sequence);
		return Group.Producer.make(inner, this.#writer);
	}

	/**
	 * Declare the end after queued writes. Existing groups may still be written; new groups
	 * are refused. Idempotent.
	 */
	finish(): void {
		if (this.#finished || this.#writer.closed) return;
		this.#finished = true;
		void this.#writer.pump.run(() => {
			if (!this.#writer.aborted) this.#inner.finish();
		});
	}

	/** Abort the track and its open groups at once, rejecting pending writes. Idempotent. */
	abort(error: Error): void {
		if (this.#writer.closed) return;
		this.#writer.closed = true;
		this.#writer.aborted = error;
		this.#inner.abort(error);
	}

	/** Release this producer: drain a declared end, or cancel when nothing finished it. Idempotent. */
	close(): void {
		if (this.#writer.closed) return;
		this.#writer.closed = true;
		if (this.#finished) {
			void this.#writer.pump.run(() => this.#inner.close());
		} else {
			this.#inner.close();
			this.#writer.aborted = this.#inner.closed.peek() ?? undefined;
		}
	}

	#writable(): void {
		if (this.#finished || this.#writer.closed) throw this.#writer.aborted ?? new Error("track is closed");
	}

	// Refuse a sequence this track already allocated or cannot represent.
	#reserve(sequence: bigint): void {
		checkGroup(sequence);
		if (sequence < this.#next) throw new Failure("reuse");
	}
}

/**
 * Subscriber for one physical track.
 *
 * Grouped authentication failure ends the track and rejects pending reads with {@link Failure}
 * `authentication`.
 */
export class Consumer {
	/** The physical track name. */
	readonly name: string;

	#inner: NetTrack.Subscriber;
	#reader: Reader;
	private constructor(inner: NetTrack.Subscriber, key: TrackKey) {
		this.#inner = inner;
		this.name = inner.name;
		this.#reader = { pump: new Pump(), key, terminal: new Terminal(inner) };
	}

	/** @internal */
	static make(inner: NetTrack.Subscriber, key: TrackKey): Consumer {
		return new Consumer(inner, key);
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
	 * Unsubscribe, releasing track demand. As in `@moq/net`, groups already received end
	 * after the frames they hold. Idempotent.
	 */
	close(): void {
		this.#reader.terminal.unsubscribe();
	}
}
