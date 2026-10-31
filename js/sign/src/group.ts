/**
 * Signing writer and verifying reader for one group.
 *
 * @module
 */
import { type Group as NetGroup, Time } from "@moq/net";
import { Failure } from "./failure.ts";
import type { PublicKey, SecretKey } from "./key.ts";
import type { Pump } from "./pump.ts";
import type { Scope } from "./scope.ts";

/** A grouped frame whose signature verified. */
export interface Frame {
	/** Presentation timestamp at the track's timescale, or `undefined` when untimed. Signed. */
	timestamp?: Time.Timestamp;
	/** The application bytes, without the signature. */
	payload: Uint8Array;
}

/** State one track producer shares with its groups. @internal */
export interface Writer {
	// One signature in flight across the track, so net writes land in call order.
	pump: Pump;
	key: SecretKey;
	scope: Scope;
	// The track's timescale, which every timestamp is signed at; undefined when untimed.
	timescale?: Time.Timescale;
	// Set by release or abort: no new writes, including on existing groups.
	closed: boolean;
	aborted?: Error;
}

/**
 * Exclusive writer for one group, signing each frame at the index it lands on.
 *
 * Writes run through the track's serial pump, so each frame's index is its net write order.
 */
export class Producer {
	/** The group's sequence number. */
	readonly sequence: number;

	#inner: NetGroup.Producer;
	#writer: Writer;
	// Frames written so far: the next frame's index.
	#next = 0;
	#closed = false;

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
	 * Sign `payload` at the next frame index and write it with the signature appended.
	 *
	 * The bytes are copied, so the caller may reuse its buffer. Rejects with the net error if
	 * the group is closed, the timestamp does not match the track, or the frame is too large.
	 */
	async writeFrame(timestamp: Time.Timestamp | undefined, payload: Uint8Array): Promise<void> {
		if (this.#closed || this.#writer.closed) throw this.#writer.aborted ?? new Error("group is closed");
		const owned = payload.slice();
		// Signed at the track's timescale, rounded as the wire rounds it, and written that way so a
		// local reader sees the signed value too. A frame whose timedness disagrees with the track
		// is left for the net write to refuse.
		const scale = this.#writer.timescale;
		const at =
			timestamp && scale !== undefined ? new Time.Timestamp(Math.round(timestamp.as(scale)), scale) : timestamp;
		return this.#writer.pump.run(async () => {
			this.#check();
			const body = await this.#writer.scope.sign(this.#writer.key, this.sequence, this.#next, at, owned);
			this.#check();
			this.#inner.writeFrame({ payload: body, timestamp: at });
			this.#next += 1;
		});
	}

	#check(): void {
		if (this.#writer.aborted) throw this.#writer.aborted;
		if (this.#inner.isClosed) throw new Error("group is closed");
	}

	/** Finish the group after every pending write. Idempotent; no new writes are accepted. */
	finish(): void {
		if (this.#closed) return;
		this.#closed = true;
		void this.#writer.pump.run(() => this.#inner.finish());
	}

	/** Abort at once with `error`; a finished or aborted group keeps its outcome. */
	abort(error: Error): void {
		this.#closed = true;
		this.#inner.abort(error);
	}
}

/**
 * Reader for one signed group.
 *
 * A frame that fails to verify ends the group with {@link Failure} `signature`: none of its
 * bytes reach the caller, and the underlying reader is released. The track keeps delivering groups.
 */
export class Consumer {
	/** The group's sequence number. */
	readonly sequence: number;

	#inner: NetGroup.Consumer;
	#key: PublicKey;
	#scope: Scope;
	#failed?: Failure;
	// Reads run one at a time, so frames verify in transport order.
	#tail: Promise<unknown> = Promise.resolve();

	private constructor(inner: NetGroup.Consumer, key: PublicKey, scope: Scope) {
		this.#inner = inner;
		this.#key = key;
		this.#scope = scope;
		this.sequence = inner.sequence;
	}

	/** @internal */
	static make(inner: NetGroup.Consumer, key: PublicKey, scope: Scope): Consumer {
		return new Consumer(inner, key, scope);
	}

	/**
	 * Read the next verified frame, or `undefined` at the end of the group.
	 *
	 * Rejects with {@link Failure} `signature` once a frame has failed to verify, or a net error.
	 */
	readFrame(): Promise<Frame | undefined> {
		const result = this.#tail.then(() => this.#read());
		this.#tail = result.catch(() => {});
		return result;
	}

	async #read(): Promise<Frame | undefined> {
		if (this.#failed) throw this.#failed;
		const frame = await this.#inner.readFrameSequence();
		if (!frame) return undefined;
		// The frame's transport index, so a group resumed above frame 0 still verifies.
		try {
			const payload = await this.#scope.verify(
				this.#key,
				this.sequence,
				frame.sequence,
				frame.timestamp,
				frame.payload,
			);
			return { timestamp: frame.timestamp, payload };
		} catch (err) {
			if (err instanceof Failure) {
				this.#failed = err;
				this.#inner.close();
			}
			throw err;
		}
	}

	/** Stop reading this group, releasing its demand. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}
