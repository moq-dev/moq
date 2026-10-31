/**
 * Grouped-frame producer and consumer for one group identity.
 *
 * @module
 */
import type { Group as NetGroup, Time } from "@moq/net";
import { Failure } from "./failure.ts";
import { checkTimestamp, type Reader, type Writer } from "./internal.ts";
import { MAX_GROUPED_PAYLOAD, TAG_LEN } from "./limits.ts";

/** A decrypted grouped frame. */
export interface Frame {
	/** Presentation timestamp, or `undefined` when untimed. Not authenticated. */
	timestamp?: Time.Timestamp;
	/** Decrypted application bytes. */
	plaintext: Uint8Array;
}

/**
 * Exclusive writer for one group; frames are numbered from 0 in write order.
 *
 * Writes run through the track's serial pump, so each frame's index is its net write order.
 */
export class Producer {
	/** The group's sequence number. */
	readonly sequence: number;

	#inner: NetGroup.Producer;
	#writer: Writer;
	// Frames encrypted so far: the next frame's index, equal to the net group's frame count.
	#next = 0;
	#closed = false;
	#aborted?: Error;

	private constructor(inner: NetGroup.Producer, writer: Writer) {
		this.#inner = inner;
		this.#writer = writer;
		this.sequence = inner.sequence;
	}

	/** @internal */
	static make(inner: NetGroup.Producer, writer: Writer): Producer {
		return new Producer(inner, writer);
	}

	/** The group's final outcome after finish drains queued writes, or an abort. */
	get closed(): NetGroup.Producer["closed"] {
		return this.#inner.closed;
	}

	/**
	 * Encrypt `plaintext` at the next frame index and write the ciphertext.
	 *
	 * The bytes are copied, so the caller may reuse its buffer. Predictable failures (an
	 * oversize plaintext, a closed group, a timestamp the track refuses, an exhausted key) are
	 * refused before an identity is spent. Once encrypted, a failed net write aborts the group,
	 * since its transport index could no longer match the nonce. Rejects with {@link Failure}
	 * `oversize`, `exhausted`, or `identity`, or a net error.
	 */
	async writeFrame(timestamp: Time.Timestamp | undefined, plaintext: Uint8Array): Promise<void> {
		if (this.#closed || this.#writer.closed)
			throw this.#aborted ?? this.#writer.aborted ?? new Error("group is closed");
		if (plaintext.byteLength + TAG_LEN > MAX_GROUPED_PAYLOAD) throw new Failure("oversize");
		checkTimestamp(this.#writer, timestamp);
		const owned = plaintext.slice();
		return this.#writer.pump.run(() => this.#write(timestamp, owned));
	}

	async #write(timestamp: Time.Timestamp | undefined, plaintext: Uint8Array<ArrayBuffer>): Promise<void> {
		this.#check();
		const frame = this.#next;
		const iv = this.#writer.key.admitSeal(this.sequence, frame, plaintext.byteLength, MAX_GROUPED_PAYLOAD);
		this.#next = frame + 1;
		try {
			const payload = await this.#writer.key.encrypt(iv, plaintext);
			this.#check();
			this.#inner.writeFrame({ payload, timestamp });
		} catch (err) {
			const error = err instanceof Error ? err : new Error(String(err));
			this.#abort(error);
			throw error;
		}
	}

	#check(): void {
		const aborted = this.#aborted ?? this.#writer.aborted;
		if (aborted) throw aborted;
		if (this.#inner.isClosed) throw new Error("group is closed");
	}

	/**
	 * Finish the group after every pending write. Idempotent; no new writes are accepted.
	 */
	finish(): void {
		if (this.#closed) return;
		this.#closed = true;
		void this.#writer.pump.run(() => {
			if (!this.#aborted) this.#inner.finish();
		});
	}

	/** Abort at once, rejecting pending writes; a finished or aborted group keeps its outcome. */
	abort(error: Error): void {
		if (this.#closed) return;
		this.#abort(error);
	}

	#abort(error: Error): void {
		this.#closed = true;
		this.#aborted ??= error;
		this.#inner.abort(error);
	}
}

/**
 * Reader for one protected group.
 *
 * A grouped authentication failure ends the track: pending reads reject with
 * {@link Failure} `authentication` and the underlying subscription is released.
 */
export class Consumer {
	/** The group's sequence number. */
	readonly sequence: number;

	#inner: NetGroup.Consumer;
	#reader: Reader;
	// Reads run one at a time, so frames open in transport order.
	#tail: Promise<unknown> = Promise.resolve();

	private constructor(inner: NetGroup.Consumer, reader: Reader) {
		this.#inner = inner;
		this.#reader = reader;
		this.sequence = inner.sequence;
	}

	/** @internal */
	static make(inner: NetGroup.Consumer, reader: Reader): Consumer {
		return new Consumer(inner, reader);
	}

	/**
	 * Read the next decrypted frame, or `undefined` at the end of the group.
	 *
	 * Rejects with {@link Failure} `authentication`, which ends the track, `identity` for a
	 * frame index past 32 bits, which does too, `oversize` or `exhausted` as for open, or a
	 * net error from the underlying group.
	 */
	readFrame(): Promise<Frame | undefined> {
		const result = this.#tail.then(() => this.#read());
		this.#tail = result.catch(() => {});
		return result;
	}

	async #read(): Promise<Frame | undefined> {
		this.#check();
		let frame: Awaited<ReturnType<NetGroup.Consumer["readFrameSequence"]>>;
		try {
			frame = await this.#inner.readFrameSequence();
		} catch (err) {
			this.#check();
			throw err;
		}
		this.#check();
		if (!frame) return undefined;

		// The frame's transport index is the nonce half, so a group resumed above frame 0 still opens.
		try {
			const { key, pump } = this.#reader;
			const iv = key.admitOpen(this.sequence, frame.sequence, frame.payload.byteLength, MAX_GROUPED_PAYLOAD);
			// A failure queued ahead of this open ends it before any crypto runs.
			const plaintext = await pump.run(() => {
				this.#check();
				return key.decrypt(iv, frame.payload);
			});
			this.#check();
			return { timestamp: frame.timestamp, plaintext };
		} catch (err) {
			if (err instanceof Failure && (err.code === "authentication" || err.code === "identity")) {
				this.#reader.terminal.fail();
				this.#inner.close();
			}
			throw err;
		}
	}

	// Once the track has failed, drop this group's net reader too, releasing its demand.
	#check(): void {
		const failed = this.#reader.terminal.failed;
		if (!failed) return;
		this.#inner.close();
		throw failed;
	}

	/** Stop reading this group, releasing its demand. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}
