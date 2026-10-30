import { Group } from "@moq/net";

/** Frames (header included) in one group before a new group is forced, matching the Snapshot cap. Kept well below the net per-group cap so a roll always precedes GroupTooLarge. */
const MAX_GROUP_FRAMES = 1024;

// A restatement changes nothing on commit; it only opens a group.
type Edit = { push: string } | { pop: number } | { restate: true };

/** Options for an {@link Encoder}. */
export interface Config {
	/**
	 * Encode edits as ops appended to the group's header. `false` restates the window in a new
	 * single-frame group for every edit.
	 *
	 * A group of ops runs until a joiner wants a fresh one, judged by the track's `refresh` ratio
	 * (see {@link Encoder.snapshot}), or until it reaches the frame or byte cap. The window's
	 * counterpart to Snapshot's `deltas`. Defaults to `true`.
	 */
	ops?: boolean;

	/**
	 * Maximum records retained and repeated in a group checkpoint.
	 *
	 * By default a header repeats the complete window. A bound keeps checkpoints finite for an
	 * unbounded window: readers following every group retain earlier records, while one joining a
	 * later group receives one `skip` for the omitted prefix.
	 */
	checkpointRecords?: number;
}

/** One encoded frame, and the group boundary it implies. */
export interface Encoded {
	/** The frame payload. */
	payload: Uint8Array;

	/**
	 * Whether this frame is a group header, which must open a new group.
	 *
	 * The encoder decides this, never the caller: the frame and byte caps force a new group
	 * independently of which edit was requested.
	 */
	keyframe: boolean;
}

/**
 * An encoded frame the caller has not yet acknowledged writing.
 *
 * Write the frame, then {@link commit}. The edit is staged until commit, so leaving the frame
 * uncommitted keeps the window unchanged and makes the next frame open a new group.
 */
export interface Pending extends Encoded {
	/** Acknowledge that the frame reached the wire, applying its edit to the retained window. */
	commit(): void;
}

/**
 * Encodes window edits into frame payloads, deciding where the group boundaries fall.
 *
 * The track-free core of {@link Producer}. It owns the retained window, so it can restate it
 * whenever a group rolls; that restatement is the whole point of the mode, and is what an
 * append-only log cannot do.
 *
 * @public
 */
export class Encoder<T> {
	#ops: boolean;
	#checkpointRecords?: number;

	// The decodable checkpoint suffix. Without a checkpoint bound this is the complete window.
	#window: string[] = [];
	// Absolute index of the oldest logically retained record.
	#offset = 0;
	// Absolute index of #window[0], which may follow #offset in checkpoint mode.
	#start = 0;

	// Bytes charged to this group, header included.
	#groupBytes = 0;
	#groupFrames = 0;
	// Whether the next frame must be a header after a lost frame.
	#resync = true;
	// Whether the frame from the last push/pop is still unacknowledged.
	#pending = false;
	// Bumped per frame handed out, so a commit arriving after the encoder moved on cannot clear the
	// flag belonging to a newer frame.
	#generation = 0;

	constructor(config: Config = {}) {
		this.#ops = config.ops ?? true;
		this.#checkpointRecords = config.checkpointRecords;
		if (
			this.#checkpointRecords !== undefined &&
			(!Number.isSafeInteger(this.#checkpointRecords) || this.#checkpointRecords <= 0)
		) {
			throw new Error("checkpointRecords must be a positive safe integer");
		}
	}

	/** The retained checkpoint suffix, oldest first. This is complete unless bounded in the config. */
	get window(): T[] {
		return this.#window.map((text) => JSON.parse(text) as T);
	}

	/** Absolute index of the oldest retained record. */
	get offset(): number {
		return this.#offset;
	}

	/** Absolute index the next pushed record will take. */
	get end(): number {
		return this.#start + this.#window.length;
	}

	/**
	 * Discard group-local state, so the next edit opens a new group with a header.
	 *
	 * Call this whenever the caller closes the current group behind the encoder's back.
	 */
	reset(): void {
		this.#resyncGroup();
	}

	/** Discard group-local state after an encoded frame did not reach the wire. */
	#resyncGroup(): void {
		this.#groupBytes = 0;
		this.#groupFrames = 0;
		this.#resync = true;
		this.#pending = false;
		this.#generation += 1;
	}

	/** Append one record to the back of the window. */
	push(value: T): Pending {
		if (this.#pending) this.#resyncGroup();
		if (this.end >= Number.MAX_SAFE_INTEGER) throw new Error("window index exceeds the safe integer range");

		// Encode before touching the window, so a value that can't be serialized leaves the encoder
		// exactly as it was.
		const text: string | undefined = JSON.stringify(value);
		if (text === undefined) {
			throw new Error("record is not representable as JSON");
		}

		const edit: Edit = { push: text };
		if (this.#resync || !this.#opAllowed()) {
			return this.#emitPushHeader(text, edit);
		}

		const payload = this.#emitOp(`{"push":${text}}`);
		if (payload) return this.#pendingFrame(payload, false, edit);
		return this.#emitPushHeader(text, edit);
	}

	/**
	 * Drop `count` records from the front of the window.
	 *
	 * Returns `undefined` when there is nothing to drop, so a caller can trim unconditionally.
	 * Clamped to what the window holds.
	 */
	pop(count: number): Pending | undefined {
		if (!Number.isSafeInteger(count) || count < 0) {
			throw new Error("pop count must be a nonnegative safe integer");
		}
		if (this.#pending) this.#resyncGroup();

		const dropped = Math.min(count, this.end - this.#offset);
		if (dropped <= 0) return undefined;

		const offset = this.#offset + dropped;
		const edit: Edit = { pop: dropped };
		if (this.#resync || !this.#opAllowed()) {
			return this.#emitPopHeader(offset, edit);
		}

		const payload = this.#emitOp(`{"pop":${dropped}}`);
		if (payload) return this.#pendingFrame(payload, false, edit);
		return this.#emitPopHeader(offset, edit);
	}

	/**
	 * Restate the retained window in a header, which must open a new group.
	 *
	 * For a caller that opens groups itself: call it when a joiner wants a fresh group, such as when
	 * the track's `demand().wanted` passes the largest group written, so the joiner reads the window
	 * without the ops before it. The frame is a {@link Pending} like any other.
	 */
	snapshot(): Pending {
		return this.#emitHeader(this.#offset, this.#start, this.#window, { restate: true });
	}

	/** Whether the pending edit may ride as an op in the open group, which is under its frame cap. */
	#opAllowed(): boolean {
		return this.#ops && this.#groupFrames > 0 && this.#groupFrames < MAX_GROUP_FRAMES;
	}

	/** Encode an already-serialized op into the open group, if its header remains cached. */
	#emitOp(text: string): Uint8Array | undefined {
		const payload = new TextEncoder().encode(text);

		this.#groupFrames += 1;
		this.#groupBytes += payload.length;
		if (this.#groupBytes > Group.MAX_GROUP_CACHE_BYTES) {
			this.#resyncGroup();
			return undefined;
		}

		return payload;
	}

	#emitPushHeader(text: string, edit: Edit): Pending {
		const records = [...this.#window, text];
		const skip = this.#checkpointRecords === undefined ? 0 : Math.max(records.length - this.#checkpointRecords, 0);
		return this.#emitHeader(this.#offset, this.#start + skip, records.slice(skip), edit);
	}

	#emitPopHeader(offset: number, edit: Edit): Pending {
		const skip = Math.min(Math.max(offset - this.#start, 0), this.#window.length);
		return this.#emitHeader(offset, this.#start + skip, this.#window.slice(skip), edit);
	}

	/** Emit a checkpoint header and open a new group. */
	#emitHeader(offset: number, start: number, window: string[], edit: Edit): Pending {
		// The records are already JSON text, so splice them in rather than re-encoding each one; the
		// bytes a reader sees are then identical to what the matching push carried.
		const checkpoint = start === offset ? "" : `,"start":${start}`;
		const text = `{"offset":${offset}${checkpoint},"records":[${window.join(",")}]}`;
		const payload = new TextEncoder().encode(text);
		if (payload.length > Group.MAX_GROUP_CACHE_BYTES) {
			throw new Error("window header exceeds the group cache limit");
		}

		this.#groupBytes = payload.length;
		this.#groupFrames = 1;
		this.#resync = false;

		return this.#pendingFrame(payload, true, edit);
	}

	#pendingFrame(payload: Uint8Array, keyframe: boolean, edit: Edit): Pending {
		this.#pending = true;
		const generation = ++this.#generation;
		const encoder = this;

		return {
			payload,
			keyframe,
			commit() {
				if (encoder.#generation !== generation || !encoder.#pending) return;
				encoder.#commit(edit);
				encoder.#pending = false;
			},
		};
	}

	/** Apply an edit after its encoded frame reached the wire. */
	#commit(edit: Edit): void {
		if ("push" in edit) {
			this.#window.push(edit.push);
			if (this.#checkpointRecords !== undefined && this.#window.length > this.#checkpointRecords) {
				const dropped = this.#window.length - this.#checkpointRecords;
				this.#window.splice(0, dropped);
				this.#start += dropped;
			}
		} else if ("pop" in edit) {
			const offset = this.#offset + edit.pop;
			const stored = Math.min(Math.max(offset - this.#start, 0), this.#window.length);
			this.#window.splice(0, stored);
			this.#start += stored;
			this.#offset = offset;
		}
	}
}
