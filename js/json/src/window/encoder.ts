import { DEFAULT_MAX_FRAME_SIZE, Encoder as Flate } from "@moq/flate";
import { Group } from "@moq/net";

/** Frames (header included) in one group before a new group is forced, matching the Snapshot cap. */
const MAX_GROUP_FRAMES = 256;

/** Op ratio used when {@link ProducerConfig.opRatio} is left unset. */
export const DEFAULT_OP_RATIO = 8;

type Edit = { push: string } | { pop: number };

/** Options shared by an {@link Encoder} and the {@link Producer} that wraps one. */
export interface ProducerConfig {
	/**
	 * How much the ops in a group may cost before a fresh group is emitted.
	 *
	 * A new group opens once the pushes and pops *already written* exceed `opRatio` times the size of
	 * the group's header frame. The pending op is excluded, so the one that tips the group over budget
	 * still lands. `0` disables ops entirely, so every edit is its own single-frame group.
	 *
	 * The window's counterpart to Snapshot's `deltaRatio`. Defaults to `8`.
	 */
	opRatio?: number;

	/**
	 * Compress each group as one sync-flushed `deflate-raw` stream, so every op reuses the header and
	 * the ops before it as context. A {@link Decoder} reading the frames must set the same flag.
	 * Defaults to `false`.
	 */
	compression?: boolean;

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
	 * The encoder decides this, never the caller: the op budget and the frame cap force a new group
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
	#opRatio: number;
	#compress: boolean;
	#checkpointRecords?: number;

	// The decodable checkpoint suffix. Without a checkpoint bound this is the complete window.
	#window: string[] = [];
	// Absolute index of the oldest logically retained record.
	#offset = 0;
	// Absolute index of #window[0], which may follow #offset in checkpoint mode.
	#start = 0;

	#flate?: Flate;
	// Bytes of pushes and pops in this group, excluding its header frame.
	#opBytes = 0;
	// Reference size the op budget is measured against: this group's header frame.
	#headerLen = 0;
	#groupFrames = 0;
	// Whether the next frame must be a header after a lost frame.
	#resync = true;
	// Whether the frame from the last push/pop is still unacknowledged.
	#pending = false;
	// Bumped per frame handed out, so a commit arriving after the encoder moved on cannot clear the
	// flag belonging to a newer frame.
	#generation = 0;

	constructor(config: ProducerConfig = {}) {
		this.#opRatio = config.opRatio ?? DEFAULT_OP_RATIO;
		if (!Number.isSafeInteger(this.#opRatio) || this.#opRatio < 0 || this.#opRatio > 0xffffffff) {
			throw new Error("opRatio must be an unsigned 32-bit integer");
		}
		this.#compress = config.compression ?? false;
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

	/** Discard group-local state after an encoded frame did not reach the wire. */
	#resyncGroup(): void {
		this.#flate = undefined;
		this.#opBytes = 0;
		this.#headerLen = 0;
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

	/** Whether the pending edit may ride as an op in the open group. */
	#opAllowed(): boolean {
		return (
			this.#opRatio !== 0 &&
			this.#groupFrames > 0 &&
			this.#groupFrames < MAX_GROUP_FRAMES &&
			this.#opBytes <= this.#opRatio * this.#headerLen
		);
	}

	/** Compress an already-serialized op into the open group, if its header remains cached. */
	#emitOp(text: string): Uint8Array | undefined {
		const bytes = new TextEncoder().encode(text);
		if (bytes.length > DEFAULT_MAX_FRAME_SIZE) {
			throw new Error("window frame exceeds the decoder's decompressed size limit");
		}
		const payload = this.#flate ? this.#flate.frame(bytes) : bytes;

		this.#opBytes += payload.length;
		this.#groupFrames += 1;
		if (this.#headerLen + this.#opBytes > Group.MAX_GROUP_CACHE_BYTES) {
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
		const bytes = new TextEncoder().encode(text);
		if (bytes.length > DEFAULT_MAX_FRAME_SIZE) {
			throw new Error("window header exceeds the decoder's decompressed size limit");
		}

		// A fresh per-group encoder (cold window), with the header as frame 0.
		const flate = this.#compress ? new Flate() : undefined;
		const payload = flate ? flate.frame(bytes) : bytes;
		if (payload.length > Group.MAX_GROUP_CACHE_BYTES) {
			throw new Error("window header exceeds the group cache limit");
		}

		this.#flate = flate;
		this.#headerLen = payload.length;
		this.#opBytes = 0;
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
		} else {
			const offset = this.#offset + edit.pop;
			const stored = Math.min(Math.max(offset - this.#start, 0), this.#window.length);
			this.#window.splice(0, stored);
			this.#start += stored;
			this.#offset = offset;
		}
	}
}
