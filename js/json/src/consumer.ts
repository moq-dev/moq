import type * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import type * as z from "zod/mini";

import { merge } from "./diff.ts";

/**
 * Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
 *
 * Always jumps to the newest group, reads its snapshot, and applies deltas in order, yielding
 * the reconstructed value after each frame. A late joiner never needs older groups.
 */
export class Consumer<T> {
	#track: Moq.Track;
	#schema?: z.ZodMiniType<T>;

	#group?: Moq.Group;
	#current?: unknown;
	#framesRead = 0;

	constructor(track: Moq.Track, schema?: z.ZodMiniType<T>) {
		this.#track = track;
		this.#schema = schema;
	}

	/** Get the next reconstructed value, or `undefined` once the track ends. */
	async next(): Promise<T | undefined> {
		for (;;) {
			// Jump to the newest group, discarding any older ones, and reset reconstruction.
			const groups = this.#track.state.groups.peek();
			if (groups.length > 0 && groups.at(-1) !== this.#group) {
				while (groups.length > 1) groups.shift()?.close();
				this.#group = groups[0];
				this.#current = undefined;
				this.#framesRead = 0;
			}

			const group = this.#group;
			if (!group) {
				const closed = this.#track.state.closed.peek();
				if (closed instanceof Error) throw closed;
				if (closed) return undefined;

				await Signal.race(this.#track.state.groups, this.#track.state.closed);
				continue;
			}

			// Frame 0 of a group is a snapshot, the rest are merge patches.
			const frame = group.state.frames.peek().shift();
			if (frame) return this.#apply(frame);

			// The current group has no pending frame.
			const groupClosed = group.state.closed.peek();
			if (groupClosed) {
				if (groupClosed instanceof Error) throw groupClosed;

				// The group is exhausted; wait for a newer one.
				if (this.#group === group) this.#group = undefined;
				const closed = this.#track.state.closed.peek();
				if (closed instanceof Error) throw closed;
				if (closed) return undefined;

				await Signal.race(this.#track.state.groups, this.#track.state.closed);
				continue;
			}

			// Group open but no frame yet: wait for a frame, a newer group, or track close.
			await Signal.race(group.state.frames, this.#track.state.groups, this.#track.state.closed);
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<T> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}

	#apply(frame: Uint8Array): T {
		const parsed = JSON.parse(new TextDecoder().decode(frame));
		if (this.#framesRead === 0) {
			this.#current = parsed;
		} else {
			this.#current = merge(this.#current, parsed);
		}
		this.#framesRead += 1;

		return this.#schema ? this.#schema.parse(this.#current) : (this.#current as T);
	}
}
