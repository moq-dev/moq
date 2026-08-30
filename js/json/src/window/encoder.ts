import { Encoder as Flate } from "@moq/flate";
import { Group } from "@moq/net";

/** Frames (reset included) in one group before a reset is forced, matching the Snapshot cap. */
const MAX_GROUP_FRAMES = 256;

/** Op ratio used when {@link ProducerConfig.opRatio} is left unset. */
export const DEFAULT_OP_RATIO = 8;

/** One frame: what the publisher did to its window. */
export type Op<T> = { reset: { offset: number; records: T[] } } | { push: T } | { pop: number };

/** Options shared by an {@link Encoder} and the {@link Producer} that wraps one. */
export interface ProducerConfig {
	/**
	 * How much the ops in a group may cost before a fresh reset is emitted.
	 *
	 * A new group opens once the pushes and pops *already written* exceed `opRatio` times the size of
	 * the group's reset frame. The pending op is excluded, so the one that tips the group over budget
	 * still lands. `0` disables ops entirely, so every edit is its own single-frame reset.
	 *
	 * The window's counterpart to Snapshot's `deltaRatio`. Defaults to `8`.
	 */
	opRatio?: number;

	/**
	 * Compress each group as one sync-flushed `deflate-raw` stream, so every op reuses the reset and
	 * the ops before it as context. A {@link Decoder} reading the frames must set the same flag.
	 * Defaults to `false`.
	 */
	compression?: boolean;
}

/** One encoded frame, and the group boundary it implies. */
export interface Encoded {
	/** The frame payload. */
	payload: Uint8Array;

	/**
	 * Whether this frame is a reset, which must open a new group.
	 *
	 * The encoder decides this, never the caller: the op budget and the frame cap force a reset
	 * independently of which edit was requested.
	 */
	keyframe: boolean;
}

/**
 * An encoded frame the caller has not yet acknowledged writing.
 *
 * Write the frame, then {@link commit}. A frame that never reaches the wire leaves the consumer's
 * view behind the publisher's window, so leaving it uncommitted resets the encoder and the next
 * frame restates the whole window.
 *
 * The window itself is not rolled back. It is the publisher's truth and the edit really happened;
 * only the consumer's knowledge of it is lost, which the next reset repairs.
 */
export interface Pending extends Encoded {
	/** Acknowledge that the frame reached the wire, keeping the encoder's state. */
	commit(): void;
}

/**
 * Encodes window edits into frame payloads, deciding where the group boundaries fall.
 *
 * The track-free core of {@link Producer}. It owns the retained window, so it can restate it
 * whenever a group rolls; that restatement is the whole point of the mode, and is what an
 * append-only log cannot do.
 */
export class Encoder<T> {
	#opRatio: number;
	#compress: boolean;

	// The retained window. Records are stored as encoded JSON text so a reset restates exactly the
	// bytes a push would have carried, and so re-encoding never re-visits the caller's value.
	#window: string[] = [];
	// Absolute index of #window[0]. Only a reset puts this on the wire.
	#offset = 0;

	#flate?: Flate;
	// Bytes of pushes and pops in this group, excluding its reset frame.
	#opBytes = 0;
	// Reference size the op budget is measured against: this group's reset frame.
	#resetLen = 0;
	#groupFrames = 0;
	// Whether the next frame must be a reset, after a lost frame or a caller-driven roll.
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
	}

	/** The retained window, oldest first. */
	get window(): T[] {
		return this.#window.map((text) => JSON.parse(text) as T);
	}

	/** Absolute index of the oldest retained record. */
	get offset(): number {
		return this.#offset;
	}

	/** Absolute index the next pushed record will take. */
	get end(): number {
		return this.#offset + this.#window.length;
	}

	/**
	 * Force the next frame to be a reset.
	 *
	 * Call this whenever the caller closes the current group behind the encoder's back. The window
	 * survives: the reset restates it in full anyway.
	 */
	reset(): void {
		this.#flate = undefined;
		this.#opBytes = 0;
		this.#resetLen = 0;
		this.#groupFrames = 0;
		this.#resync = true;
		this.#pending = false;
	}

	/** Append one record to the back of the window. */
	push(value: T): Pending {
		this.#resync ||= this.#lost();
		if (this.end >= Number.MAX_SAFE_INTEGER) throw new Error("window index exceeds the safe integer range");

		// Encode before touching the window, so a value that can't be serialized leaves the encoder
		// exactly as it was.
		const text = JSON.stringify(value);
		if (text === undefined) {
			throw new Error("record is not representable as JSON");
		}

		this.#window.push(text);
		if (this.#resync || !this.#opAllowed()) return this.#emitReset();

		return this.#emitOp(`{"push":${text}}`);
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
		this.#resync ||= this.#lost();

		const dropped = Math.min(count, this.#window.length);
		if (dropped <= 0) return undefined;

		this.#window.splice(0, dropped);
		this.#offset += dropped;

		if (this.#resync || !this.#opAllowed()) return this.#emitReset();
		return this.#emitOp(`{"pop":${dropped}}`);
	}

	/** Whether the pending edit may ride as an op in the open group. */
	#opAllowed(): boolean {
		return (
			this.#opRatio !== 0 &&
			this.#groupFrames > 0 &&
			this.#groupFrames < MAX_GROUP_FRAMES &&
			this.#opBytes <= this.#opRatio * this.#resetLen
		);
	}

	/** Compress an already-serialized op into the open group, charging it to the budget. */
	#emitOp(text: string): Pending {
		const bytes = new TextEncoder().encode(text);
		const payload = this.#flate ? this.#flate.frame(bytes) : bytes;

		this.#opBytes += payload.length;
		this.#groupFrames += 1;
		if (this.#resetLen + this.#opBytes > Group.MAX_GROUP_CACHE_BYTES) return this.#emitReset();

		return this.#pendingFrame(payload, false);
	}

	/** Emit a reset restating the whole window, opening a new group. */
	#emitReset(): Pending {
		// The records are already JSON text, so splice them in rather than re-encoding each one; the
		// bytes a reader sees are then identical to what the matching push carried.
		const text = `{"reset":{"offset":${this.#offset},"records":[${this.#window.join(",")}]}}`;
		const bytes = new TextEncoder().encode(text);

		// A fresh per-group encoder (cold window), with the reset as frame 0.
		this.#flate = this.#compress ? new Flate() : undefined;
		const payload = this.#flate ? this.#flate.frame(bytes) : bytes;

		this.#resetLen = payload.length;
		this.#opBytes = 0;
		this.#groupFrames = 1;
		this.#resync = false;

		return this.#pendingFrame(payload, true);
	}

	#pendingFrame(payload: Uint8Array, keyframe: boolean): Pending {
		this.#pending = true;
		const generation = ++this.#generation;
		const encoder = this;

		return {
			payload,
			keyframe,
			commit() {
				if (encoder.#generation === generation) encoder.#pending = false;
			},
		};
	}

	/**
	 * Whether the previous frame was handed out and never committed.
	 *
	 * Clears the flag, since the caller is about to be given a reset that repairs it.
	 */
	#lost(): boolean {
		const lost = this.#pending;
		this.#pending = false;
		return lost;
	}
}
