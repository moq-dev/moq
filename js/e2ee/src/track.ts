/**
 * Grouped-frame writers and readers for one physical track.
 *
 * @module
 */
import { type Group as NetGroup, Track as NetTrack } from "@moq/net";
import { checkGroup } from "./derive.ts";
import { Failure } from "./failure.ts";
import * as Group from "./group.ts";
import type { Reader, Writer } from "./internal.ts";
import type { TrackKey } from "./key.ts";
import { MAX_GROUPED_PAYLOAD, TAG_LEN } from "./limits.ts";
import { Pump } from "./pump.ts";
import { Terminal } from "./terminal.ts";

/**
 * Exclusive producer for one physical track.
 *
 * Owns the grouped-frame key and group start namespace. Starts are allocated monotonically;
 * reuse and exhaustion are refused before encryption. Every write
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

	/**
	 * Start a group with its first frame, after every group this track holds, as
	 * {@link NetTrack.Producer.appendGroup} does: the group starts at the frame's timestamp, which
	 * a timed track converts into its timescale, rounding down. The start must be above every start
	 * this track allocated.
	 *
	 * Rejects with {@link Failure} `reuse` if its start was already allocated, `identity` if it
	 * exceeds `2^53-1`, `oversize` or `exhausted` as for {@link Group.Producer.writeFrame}, or a
	 * net error.
	 */
	appendGroup(frame: Group.Frame): Promise<Group.Producer> {
		return this.#open(frame, true);
	}

	/**
	 * Start a group with its first frame at the frame's timestamp, which must be at or above the
	 * next unallocated start, as {@link NetTrack.Producer.createGroup} does.
	 *
	 * Rejects as {@link appendGroup} does.
	 */
	createGroup(frame: Group.Frame): Promise<Group.Producer> {
		return this.#open(frame, false);
	}

	// Allocate the group's start now, so later calls queue behind it, then encrypt frame 0 and
	// open the net group with it in call order.
	async #open(frame: Group.Frame, append: boolean): Promise<Group.Producer> {
		this.#writable();
		if (frame.plaintext.byteLength + TAG_LEN > MAX_GROUPED_PAYLOAD) throw new Failure("oversize");
		// Converted before allocating, so a refused start allocates nothing. The group starts at its
		// first frame: a scale-less track's frame names the key it starts.
		const timestamp = NetTrack.rescale(this.#writer.track, frame.timestamp);
		const key = timestamp.key ?? BigInt(timestamp.value);
		this.#reserve(key);
		this.#next = key + 1n;
		const plaintext = frame.plaintext.slice();
		return this.#writer.pump.run(async () => {
			if (this.#writer.aborted) throw this.#writer.aborted;
			const iv = this.#writer.key.admitSeal(key, 0, plaintext.byteLength, MAX_GROUPED_PAYLOAD);
			const payload = await this.#writer.key.encrypt(iv, plaintext);
			if (this.#writer.aborted) throw this.#writer.aborted;
			const sealed = { payload, timestamp };
			const inner = append ? this.#inner.appendGroup(sealed) : this.#inner.createGroup(sealed);
			return Group.Producer.make(inner, this.#writer);
		});
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

	// Refuse a start this track already allocated or cannot represent.
	#reserve(start: bigint): void {
		checkGroup(start);
		if (start < this.#next) throw new Failure("reuse");
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
	 * Receive the next protected group in arrival order, or `undefined` once the track ends. Groups
	 * already held come in the subscription's Group Order, newest first unless it asks ascending.
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
