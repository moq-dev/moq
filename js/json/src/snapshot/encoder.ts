import { Group, Error as NetError } from "@moq/net";
import type * as z from "@zod/mini";

import { deepEqual, diff } from "../diff.ts";

// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced. Kept
// well below the per-group frame cap so a late joiner can always read the snapshot at frame 0.
const MAX_DELTA_FRAMES = 256;

/** Options shared by an {@link Encoder} and the {@link Producer} that wraps one. */
export interface Config<T> {
	/**
	 * Encode changes as deltas (merge patches) against the group's snapshot. `false` encodes every
	 * change as a new snapshot in its own group.
	 *
	 * A group of deltas runs until a joiner wants a fresh one, judged by the track's `refresh` ratio
	 * (see {@link Encoder.snapshot}), or until it reaches the frame or byte cap.
	 *
	 * Defaults to `true`.
	 */
	deltas?: boolean;

	/** Optional zod schema used to validate each value before publishing. */
	schema?: z.ZodMiniType<T>;

	/**
	 * Starting value for {@link Producer.mutate} before anything has been published. Required to
	 * mutate a producer that hasn't published yet (e.g. a fresh catalog); ignored once a value exists.
	 */
	initial?: T;

	/**
	 * The most bytes a group may hold, snapshot and deltas together.
	 *
	 * A delta that would pass it rolls a fresh snapshot instead, and a snapshot past it throws
	 * `FrameTooLarge`. Lowering it lets a test reach the limit without megabytes of JSON; a value
	 * above moq-net's per-group cache limit is clamped to it, since moq-net refuses a larger group
	 * anyway. Defaults to that limit.
	 */
	maxGroupBytes?: number;
}

/** One encoded frame, and the group boundary it implies. */
export interface Encoded {
	/** The frame payload: a full JSON snapshot or a merge patch. */
	payload: Uint8Array;

	/**
	 * Whether this frame is a full snapshot, which must open a new group.
	 *
	 * `true` means the caller writes it as the first frame of a fresh group; `false` means it is a
	 * merge patch that must be appended to the group the last snapshot opened.
	 *
	 * The encoder decides this, never the caller: a value that sets a field to JSON null, or whose
	 * root isn't an object, cannot be expressed as a merge patch at all, and the frame and byte caps
	 * force a snapshot independently of what the caller wanted.
	 */
	keyframe: boolean;
}

/**
 * An encoded frame the caller has not yet acknowledged writing, returned by {@link Encoder.update}.
 *
 * Write the frame, then {@link commit}. The frame's value is staged until commit, so one that is
 * never committed leaves {@link Encoder.value} at the last value that reached the wire, and the
 * encoder resynchronizes on the next {@link Encoder.update}: it starts a fresh snapshot rather than
 * emitting deltas against a group no consumer received.
 *
 * The group state is a recovery, not a rollback. A consumer may hold the group the lost frame was
 * meant for, so a snapshot is the only sound way back. Forgetting to
 * commit a frame that *was* written therefore costs one redundant snapshot and leaves the value a
 * step behind, but never corrupts the stream.
 */
export interface Pending extends Encoded {
	/**
	 * Acknowledge that the frame reached the wire, applying its value to the baseline.
	 *
	 * Only call this once the write has actually succeeded. Committing a frame that failed to write
	 * is the one thing that corrupts the stream.
	 */
	commit(): void;
}

/**
 * Encodes a JSON value into frame payloads, choosing snapshots and deltas automatically.
 *
 * The track-free core of {@link Producer}: it decides *what bytes go in a frame* and *where the
 * group boundaries fall*, and leaves writing them to the caller. Reach for it when something else
 * already owns the track.
 *
 * Frames must reach the wire in the order they were encoded, and a frame with
 * {@link Encoded.keyframe} set must open a new group: the merge patches depend on it. {@link update} hands back a {@link Pending} rather than a bare
 * {@link Encoded} so a frame that never reaches the wire can't silently desync the encoder: leave it
 * uncommitted and the next update resynchronizes with a fresh snapshot.
 *
 * (The Rust `moq-json` encoder does the same thing through `Drop`, so it resynchronizes the moment
 * the frame is discarded. JavaScript has no destructor, so the check happens on the next update
 * instead. The resulting stream is identical either way.)
 *
 * If the caller closes a group for its own reasons, call
 * {@link reset} so the next value is encoded as a snapshot.
 */
export class Encoder<T> {
	#config: Config<T>;
	#maxGroupBytes: number;

	// The last committed value, normalized through JSON so it matches what landed on the wire. The
	// baseline every delta is diffed against, and `undefined` until the first snapshot commits.
	#last?: unknown;

	// Bytes charged to the current group, snapshot included.
	#groupBytes = 0;
	// Frames emitted into the current group, snapshot included.
	#groupFrames = 0;

	// Whether the frame from the last {@link update} is still unacknowledged. An uncommitted frame
	// never reached the wire, so the next update resynchronizes before encoding anything new.
	#pending = false;
	// Bumped for each frame handed out, so a commit that arrives after the encoder has moved on can
	// tell that it is acknowledging a frame that is no longer the outstanding one.
	#generation = 0;

	// Whether the next frame has to be a full snapshot, because a frame was lost or the caller closed
	// the group. Kept separate from {@link #last} so a resync doesn't erase the value: that field is
	// also what {@link Producer.mutate} seeds an edit from, and dropping it there would publish a
	// document with every other field missing.
	#resync = false;

	constructor(config: Config<T> = {}) {
		this.#config = config;
		this.#maxGroupBytes = Math.min(config.maxGroupBytes ?? Infinity, Group.MAX_GROUP_CACHE_BYTES);
	}

	/**
	 * A copy of the last committed value, or `undefined` before the first snapshot commits.
	 *
	 * A frame left uncommitted never reached the wire, so its value never lands here. Copied rather
	 * than shared: the baseline is what the next delta is diffed against, so a caller
	 * mutating it in place would make a later change look unchanged and never reach consumers.
	 */
	get value(): T | undefined {
		return this.#last === undefined ? undefined : (structuredClone(this.#last) as T);
	}

	/**
	 * Force the next {@link update} to emit a full snapshot, even for an unchanged value.
	 *
	 * Call this whenever the caller closes the current group behind the encoder's back. Without it
	 * the next value may be encoded as a delta against a baseline that the new group doesn't carry.
	 *
	 * {@link value} survives: the snapshot republishes it in full anyway, and it is what a caller
	 * editing in place starts from.
	 */
	reset(): void {
		this.#groupBytes = 0;
		this.#groupFrames = 0;
		this.#resync = true;
		this.#pending = false;
	}

	/**
	 * Encode a new value, as a snapshot or a delta.
	 *
	 * Returns `undefined` when the value is unchanged from the last one committed, so nothing needs
	 * to be written. Otherwise the frame comes back as a {@link Pending} the caller writes and then
	 * commits; leaving one uncommitted resynchronizes the encoder here, on the next call.
	 */
	update(value: T): Pending | undefined {
		// The previous frame was never acknowledged, so it never reached the wire. Resynchronize so
		// this value goes out as a fresh snapshot rather than as a patch against a state no consumer
		// holds.
		if (this.#pending) this.reset();

		const valid = this.#config.schema ? this.#config.schema.parse(value) : value;

		// Serialize once; parse it back to a normalized JSON value for diffing and comparison
		// (dropping `undefined` fields, matching what lands on the wire).
		const text = JSON.stringify(valid);
		if (text === undefined) {
			// `JSON.stringify` yields undefined for a top-level undefined, function, or symbol. Reject it
			// here rather than letting it reach the wire as an unparseable frame.
			throw new Error("value is not representable as JSON");
		}

		const json = JSON.parse(text);

		// A resync has to emit even for an unchanged value: the frame that carried it may never have
		// landed, so the consumer's state is unknown.
		if (!this.#resync && this.#last !== undefined && deepEqual(this.#last, json)) return undefined;

		const delta = this.#delta(json);

		// The baseline advances on commit, so it never runs ahead of the wire.
		if (delta) {
			// A delta is only readable while the group still holds the snapshot it applies to.
			// Admitting a patch that pushes the group past that budget would abort it
			// (`GroupTooLarge`), leaving a late subscriber with no value, so fall through to a fresh
			// snapshot instead.
			if (this.#groupBytes + delta.length <= this.#maxGroupBytes) {
				this.#groupBytes += delta.length;
				this.#groupFrames += 1;
				return this.#pend({ payload: delta, keyframe: false }, json);
			}
			this.reset();
		}

		const payload = this.#snapshot(new TextEncoder().encode(text));
		return this.#pend({ payload, keyframe: true }, json);
	}

	/**
	 * Restate the last committed value as a full snapshot, which must open a new group.
	 *
	 * For a caller that opens groups itself: call it when a joiner wants a fresh group, such as when
	 * the track's `demand().wanted` passes the largest group written, so the joiner reads the value
	 * without the deltas before it. Returns `undefined` before the first value, when there is nothing
	 * to restate. The frame is a {@link Pending} like any other.
	 */
	snapshot(): Pending | undefined {
		if (this.#last === undefined) return undefined;
		const payload = this.#snapshot(new TextEncoder().encode(JSON.stringify(this.#last)));
		return this.#pend({ payload, keyframe: true }, this.#last);
	}

	// Hand a frame to the caller, marking it unacknowledged until they commit, when `value` becomes
	// the baseline.
	#pend(encoded: Encoded, value: unknown): Pending {
		this.#pending = true;
		const generation = ++this.#generation;

		return {
			...encoded,
			commit: () => {
				// A caller that starts the next update before this frame settles has already made the
				// encoder resynchronize around it. Acknowledging it now would clear the flag belonging to
				// the newer frame, so a later loss of that one would go unnoticed.
				if (this.#generation !== generation) return;
				this.#pending = false;
				this.#last = value;
			},
		};
	}

	// Build a delta frame, or `undefined` to signal that a fresh snapshot should be emitted.
	#delta(json: unknown): Uint8Array | undefined {
		// A lost frame, or a group the caller closed, leaves the consumer's state unknown, so the next
		// frame has to be a full snapshot.
		if (this.#resync) return undefined;

		if (this.#config.deltas === false) return undefined;
		if (this.#last === undefined) return undefined;
		if (this.#groupFrames === 0 || this.#groupFrames >= MAX_DELTA_FRAMES) return undefined;

		const result = diff(this.#last, json);
		if (result.forcedSnapshot) return undefined;

		return new TextEncoder().encode(JSON.stringify(result.patch));
	}

	// Encode a group's snapshot (frame 0), returning its payload. A snapshot past the budget is
	// refused before any state changes, so the previous value stays readable rather than superseded
	// by a frame moq-net would refuse on write.
	#snapshot(frame: Uint8Array): Uint8Array {
		if (frame.length > this.#maxGroupBytes) throw new NetError.FrameTooLarge();
		this.#groupBytes = frame.length;
		this.#groupFrames = 1;
		this.#resync = false;
		return frame;
	}
}
