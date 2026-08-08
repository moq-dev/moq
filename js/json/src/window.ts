/**
 * Sliding-window JSON publishing over MoQ tracks: an ordered log that grows at the tail and
 * shrinks at the head, like an HLS media playlist. {@link Producer.append} adds a record as
 * content becomes available and {@link Producer.trim} drops records from the head as it stops
 * being available. The counterpart to the `Stream` module, which only ever appends, and to
 * `Snapshot`, which keeps no history at all.
 *
 * Every record has a stable **position**: the count of records ever appended before it. Positions
 * never repeat, so a record is identified across group rolls and trims, exactly like an HLS media
 * sequence number.
 *
 * On the wire each group is self-contained. Frame 0 is a snapshot of the live window
 * (`{"offset":N,"values":[...]}`) and each later frame is one operation (`{"append":V}` or
 * `{"trim":N}`). A new group rolls once the records trimmed since the snapshot outnumber those
 * still in the window, so a late joiner reads the newest group and needs nothing older.
 * Interoperable on the wire with the Rust `moq_json::window`.
 *
 * @module
 */

import { Decoder, Encoder } from "@moq/flate";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

/**
 * Maximum frames (snapshot plus operations) in one group before a snapshot is forced.
 *
 * Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot at
 * frame 0 before the group is evicted.
 */
const MAX_GROUP_FRAMES = 256;

/**
 * The largest position the wire format allows: 2^53-1.
 *
 * Past this a JSON number stops counting exactly, so two distinct positions round together and
 * lossless group switching breaks: the next append would repeat a position the consumer already
 * yielded. Matches `MAX_POSITION` in the Rust window.
 */
const MAX_POSITION = Number.MAX_SAFE_INTEGER;

/** Compress (when an encoder is given) and write one already-serialized frame. */
function writeFrame(group: Moq.Group.Producer, encoder: Encoder | undefined, payload: string): void {
	const bytes = new TextEncoder().encode(payload);
	group.writeFrame({ payload: encoder ? encoder.frame(bytes) : bytes, timestamp: Time.Timestamp.now() });
}

/** Options for a window {@link Producer}. */
export interface ProducerConfig {
	/**
	 * Compress each group as one sync-flushed `deflate-raw` stream, so operations reuse the
	 * snapshot as context and shrink sharply. A {@link Consumer} reading the track must set the
	 * same flag. Defaults to `false`.
	 */
	compression?: boolean;
}

/** Options for a window {@link Consumer}. */
export interface ConsumerConfig {
	/** Whether the track's frames are `deflate-raw` compressed. Must match the producer. Defaults to `false`. */
	compression?: boolean;
}

/** One change to the window, yielded by a {@link Consumer}. */
export type Update<T> =
	| {
			type: "append";
			/** The record's position: the count of records ever appended before it. */
			position: number;
			/** The record itself. */
			value: T;
	  }
	| {
			type: "trim";
			/** The position of the oldest record still in the window; everything before it is gone. */
			offset: number;
	  };

/** The snapshot at frame 0 of every group. */
interface Snapshot<T> {
	offset: number;
	values: T[];
}

/** One operation frame. Unknown members are ignored, so a frame carrying neither key is a no-op. */
interface Op<T> {
	append?: T;
	trim?: number;
}

/** Publishes a sliding window of JSON records to a track. */
export class Producer<T> {
	#track: Moq.Track.Producer;
	#compress: boolean;

	#group?: Moq.Group.Producer;
	// The open group's DEFLATE encoder (one window per group), present while compressing.
	#encoder?: Encoder;
	// Every record currently in the window, oldest first, kept so a new group can snapshot it.
	//
	// Stored already serialized rather than by reference: a caller that mutates an object it
	// appended would otherwise make a later snapshot emit different JSON for the same position, so
	// a consumer that saw the first group and one that joined later would disagree about a record
	// they both name. Serializing once at append pins the value, matching the Rust window's
	// `Box<RawValue>`.
	#window: string[] = [];
	// The position of `#window[0]`: how many records have been trimmed for the log's lifetime.
	#offset = 0;
	// Records trimmed since the open group's snapshot, the roll trigger.
	#trimmed = 0;
	#frames = 0;

	/** Wrap a track to publish a sliding record window into it. */
	constructor(track: Moq.Track.Producer, config: ProducerConfig = {}) {
		this.#track = track;
		this.#compress = config.compression ?? false;
	}

	/** The position of the oldest record still in the window. */
	get offset(): number {
		return this.#offset;
	}

	/** The number of records currently in the window. */
	get length(): number {
		return this.#window.length;
	}

	/** Append one record to the tail of the window, returning the position it was assigned. */
	append(value: T): number {
		const raw = JSON.stringify(value);
		if (raw === undefined) {
			throw new Error("record is not JSON-serializable");
		}

		const position = this.#offset + this.#window.length;
		// A consumer rejects an append *at* the maximum, so refuse to publish one: the window would
		// be unreadable by the very consumers it targets.
		if (position >= MAX_POSITION) {
			throw new Error(`append at position ${position} reaches the maximum ${MAX_POSITION}`);
		}
		this.#window.push(raw);

		try {
			if (this.#needsSnapshot()) {
				this.#snapshot();
			} else {
				this.#emit(`{"append":${raw}}`);
			}
		} catch (err) {
			// A publish that failed (a finished or closed track) must leave the window as it was:
			// `offset`, `length`, and the next position all read from it, so keeping a record nobody
			// received would report a window that does not exist.
			this.#window.pop();
			throw err;
		}
		return position;
	}

	/**
	 * Remove the oldest `count` records from the head of the window.
	 *
	 * A `count` of 0 does nothing. Throws if it exceeds the number of records in the window, since
	 * trimming past the head would desync every consumer's positions.
	 */
	trim(count: number): void {
		// Check integrality before the range test: `NaN` fails every comparison, so it would slip
		// through, splice nothing, and poison `#offset` (and serialize as `null`). A fraction would
		// splice nothing either, yet still emit a count both consumers reject.
		if (!Number.isSafeInteger(count) || count < 0) {
			throw new Error(`trim count must be a non-negative integer, got ${count}`);
		}
		if (count === 0) return;
		if (count > this.#window.length) {
			throw new Error(`trim of ${count} exceeds the ${this.#window.length} records in the window`);
		}

		const removed = this.#window.splice(0, count);
		this.#offset += count;
		this.#trimmed += count;

		try {
			if (this.#needsSnapshot()) {
				this.#snapshot();
			} else {
				this.#emit(`{"trim":${count}}`);
			}
		} catch (err) {
			// Same rollback as `append`: an unpublished trim must not move the head.
			this.#offset -= count;
			this.#trimmed -= count;
			this.#window.unshift(...removed);
			throw err;
		}
	}

	/** Finish the track, closing any open group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#encoder = undefined;
		this.#track.close();
	}

	/**
	 * Whether the next operation should roll a fresh snapshot group instead.
	 *
	 * Rolls once the records trimmed since the snapshot outnumber those still in the window: the
	 * group is then mostly dead weight for a new subscriber, and a fresh snapshot costs no more
	 * than what has already been trimmed.
	 */
	#needsSnapshot(): boolean {
		return !this.#group || this.#frames >= MAX_GROUP_FRAMES || this.#trimmed > this.#window.length;
	}

	/**
	 * Start a new group whose frame 0 snapshots the whole window.
	 *
	 * State moves to the new group only once its snapshot frame lands. A half-rolled group (open,
	 * but with no frame 0) would take the next append down the operation path and publish it as
	 * frame 0, which every consumer rejects as a malformed snapshot, leaving the track unrecoverable.
	 * Clearing the group instead forces the next publish to snapshot again.
	 */
	#snapshot(): void {
		// Materialize before touching any group state, so a serialization failure changes nothing.
		const payload = `{"offset":${this.#offset},"values":[${this.#window.join(",")}]}`;

		// The previous group is complete; no more frames will be appended to it.
		this.#group?.close();
		this.#group = undefined;
		this.#encoder = undefined;
		this.#frames = 0;

		const group = this.#track.appendGroup();
		const encoder = this.#compress ? new Encoder() : undefined;
		try {
			writeFrame(group, encoder, payload);
		} catch (err) {
			group.close();
			throw err;
		}

		this.#group = group;
		this.#encoder = encoder;
		this.#frames = 1;
		// Only now is the roll real, so only now does the trim debt reset. Resetting earlier would
		// let a failed roll drive `#trimmed` negative when the caller rolls back.
		this.#trimmed = 0;
	}

	/** Write one already-serialized frame into the open group. */
	#emit(payload: string): void {
		// biome-ignore lint/style/noNonNullAssertion: #needsSnapshot guarantees an open group
		writeFrame(this.#group!, this.#encoder, payload);
		this.#frames += 1;
	}
}

/**
 * Reads a sliding window of JSON records from a track, yielding each change in order.
 *
 * Starts at the newest group's snapshot, so a late joiner sees an append per record currently in
 * the window and then follows along live. Switching to a newer group is lossless: positions
 * identify what has already been yielded, so records are never repeated.
 *
 * A trim only ever retracts records this consumer was told about, so joining a window that has
 * already trimmed does not report the records it missed. Those show up instead as a jump in the
 * first append's `position`.
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decompress: boolean;

	#group?: Moq.Group.Consumer;
	// The open group's DEFLATE decoder (one window per group), built on its first frame.
	#decoder?: Decoder;
	// Frames read from the open group; frame 0 is its snapshot.
	#frames = 0;
	// The position of the next record this consumer has not yielded yet.
	#next = 0;
	// The head offset already reported through a trim, or undefined until the first snapshot
	// establishes where this consumer joined.
	#reported?: number;
	// The head offset of the group being followed, advanced by its trims.
	#head = 0;
	// The position one past the last record the group being followed has appended.
	#tail = 0;
	// Updates decoded but not yet yielded; one frame can decode a whole snapshot.
	#pending: Update<T>[] = [];

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decompress = config.compression ?? false;
	}

	/** Get the next update, or `undefined` once the track ends. */
	async next(): Promise<Update<T> | undefined> {
		for (;;) {
			const ready = this.#pending.shift();
			if (ready) return ready;

			if (!this.#group) {
				// Advance to the next group with a higher sequence number (skipping late arrivals).
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;

				// Then skip to the newest group already buffered. Each group is self-contained, so
				// an older one carries nothing the newest lacks; without this a late joiner replays
				// every cached group, emitting appends and trims for records already gone.
				//
				// Close each group skipped past. A subscriber's group is its own mirror of the
				// source (`Group.Producer.mirror` builds a fresh state per sink), so closing one
				// releases only this reader's copy and drops its share of the producer's demand.
				for (let newer = this.#track.tryNextGroup(); newer; newer = this.#track.tryNextGroup()) {
					this.#group.close();
					this.#group = newer;
				}

				this.#frames = 0;
				this.#decoder = undefined;
			}

			// Drain everything already buffered before blocking, so a late joiner catches up in one step.
			let advanced = false;
			for (let frame = this.#group.tryReadFrame(); frame !== undefined; frame = this.#group.tryReadFrame()) {
				this.#apply(frame.payload);
				advanced = true;
			}
			if (advanced) continue;

			let frame: Moq.Group.Frame | undefined;
			try {
				frame = await this.#group.readFrame();
			} catch {
				// The group was reset or we fell behind its eviction window. Resync from the next
				// group, which begins with a fresh snapshot, so no partial state is presented.
				this.#group = undefined;
				continue;
			}

			if (frame === undefined) {
				// Every group opens with a snapshot, so one that ends cleanly having delivered
				// nothing is malformed. Accepting it would let it masquerade as a clean end of
				// track and silently lose the window the publisher advertised. Rust rejects the
				// same group. (A group that *errors* before frame 0 is a different case, handled
				// above: that is a lost group, and it resyncs.)
				if (this.#frames === 0) throw new Error("a group carried no snapshot");

				// The group is exhausted; advance to the next one.
				this.#group = undefined;
				continue;
			}

			this.#apply(frame.payload);
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<Update<T>> {
		for (;;) {
			const update = await this.next();
			if (update === undefined) return;
			yield update;
		}
	}

	/** Apply one frame: frame 0 of a group is a snapshot, the rest are operations. */
	#apply(frame: Uint8Array): void {
		let payload = frame;
		if (this.#decompress) {
			this.#decoder ??= new Decoder();
			payload = this.#decoder.frame(frame);
		}
		const parsed = JSON.parse(new TextDecoder().decode(payload));

		if (this.#frames === 0) {
			this.#applySnapshot(parsed as Snapshot<T>);
		} else {
			this.#applyOp(parsed as Op<T>);
		}
		this.#frames += 1;
	}

	/** Adopt a group's snapshot, yielding only what this consumer hasn't seen. */
	#applySnapshot(snapshot: Snapshot<T>): void {
		// Every position derives from this offset. A missing or non-numeric one would make `#head`,
		// `#tail`, and every position NaN, and NaN comparisons are all false, so the consumer would
		// keep emitting appends whose positions no reader can use. Fail loudly instead.
		if (!Number.isSafeInteger(snapshot.offset) || snapshot.offset < 0) {
			throw new Error(`snapshot carries an invalid offset: ${JSON.stringify(snapshot.offset)}`);
		}
		// `values` is required, so an absent one is malformed rather than an empty window: accepting
		// it would report a head jump and continue from fabricated state, where the Rust decoder
		// fails. Both implementations must reject the same frames.
		if (!Array.isArray(snapshot.values)) {
			throw new Error("snapshot values must be an array");
		}
		const values = snapshot.values;
		// The tail matters as much as the offset: past the exact-integer limit two positions round
		// together, so the next append would repeat one the consumer already yielded.
		if (snapshot.offset + values.length > MAX_POSITION) {
			throw new Error(`snapshot spans past the maximum position ${MAX_POSITION}`);
		}
		this.#head = snapshot.offset;
		this.#tail = snapshot.offset + values.length;
		this.#reportTrim();

		values.forEach((value, index) => {
			const position = snapshot.offset + index;
			// Already yielded from an older group: the position is what makes the switch lossless.
			if (position < this.#next) return;
			this.#pushAppend(position, value);
		});
	}

	/** Apply one operation frame from the group being followed. */
	#applyOp(op: Op<T>): void {
		// A frame carries at most one operation. Applying both would append and retract in an order
		// the format never defines, so reject it instead of guessing. A frame carrying *neither* is
		// not an error: that is how an operation added by a later revision reads here.
		if ("append" in op && op.trim !== undefined) {
			throw new Error("a frame carries both an append and a trim");
		}

		// An explicit `null` is a valid record, so test for the member's presence rather than for a
		// defined value: collapsing the two would drop the record and stall the tail, shifting every
		// later position.
		if ("append" in op) {
			const position = this.#tail;
			// The snapshot bound is not enough on its own: a snapshot can land the tail exactly at
			// the limit, and appends walk it past. Rust rejects the same operation.
			if (position >= MAX_POSITION) {
				throw new Error(`append at position ${position} reaches the maximum ${MAX_POSITION}`);
			}
			this.#tail += 1;
			if (position >= this.#next) this.#pushAppend(position, op.append as T);
		}

		if (op.trim !== undefined) {
			// A non-numeric trim would coerce to 0 and silently no-op, desyncing the head from the
			// publisher's; reject it instead.
			if (typeof op.trim !== "number" || !Number.isInteger(op.trim) || op.trim < 0) {
				throw new Error(`invalid trim ${JSON.stringify(op.trim)}`);
			}
			if (this.#head + op.trim > this.#tail) {
				throw new Error(`trim of ${op.trim} exceeds the ${this.#tail - this.#head} records in the window`);
			}
			this.#head += op.trim;
			this.#reportTrim();
		}
	}

	/**
	 * Emit a trim if the followed group's head has moved past what was last reported.
	 *
	 * A trim says "records you were told about are gone", so the first snapshot only adopts its
	 * offset: a consumer joining a window that already trimmed never held those records.
	 */
	#reportTrim(): void {
		if (this.#reported === undefined) {
			this.#reported = this.#head;
			return;
		}
		if (this.#head > this.#reported) {
			this.#reported = this.#head;
			this.#pending.push({ type: "trim", offset: this.#head });
		}
	}

	/** Queue a record, advancing the yield cursor past it. */
	#pushAppend(position: number, value: T): void {
		this.#next = position + 1;
		this.#pending.push({ type: "append", position, value });
	}
}
