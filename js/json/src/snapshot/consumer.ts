import type * as Moq from "@moq/net";

import { Decoder } from "./decoder.ts";
import type { Config } from "./encoder.ts";

/**
 * Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
 *
 * A {@link Decoder} that owns its track: it reads groups, routes each frame by its position, and
 * yields the reconstructed value. A live consumer yields each update as it arrives; a consumer that
 * has fallen behind (or just joined) collapses the buffered backlog and yields only the latest
 * value. See {@link next}. When something else already owns the track, use the {@link Decoder}
 * directly.
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;
	#framesRead = 0;

	constructor(track: Moq.Track.Subscriber, config: Config<T> = {}) {
		this.#track = track;
		this.#decoder = new Decoder(config);
	}

	/**
	 * Get the next reconstructed value, or `undefined` once the track ends.
	 *
	 * Applies every frame already buffered in the group but yields only the latest reconstructed
	 * value: the intermediate reconstructions are stale, so a late joiner (or any consumer that has
	 * fallen behind) catches up to the head in one step instead of replaying every superseded state.
	 * Frames are still decoded in order (the DEFLATE window and merge patches are sequential); only
	 * the per-frame yield is skipped.
	 */
	async next(): Promise<T | undefined> {
		for (;;) {
			if (!this.#group) {
				// Advance to the next group with a higher sequence number (skipping late arrivals).
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				// The next frame is the new group's snapshot, which also restarts the decoder's window.
				this.#framesRead = 0;
			}

			// Drain every frame already buffered, keeping only the latest reconstructed value: a late
			// joiner (or any consumer that fell behind) catches up to the head in one step.
			let advanced = false;
			for (let frame = this.#group.tryReadFrame(); frame !== undefined; frame = this.#group.tryReadFrame()) {
				this.#apply(frame.payload);
				advanced = true;
			}
			if (advanced) return this.#decoder.decode();

			// Nothing buffered: block for the next frame (or the group's end).
			let frame: Moq.Group.Frame | undefined;
			try {
				frame = await this.#group.readFrame();
			} catch {
				// The group was reset or we fell behind its eviction window. Resync from
				// the next group, which begins with a fresh snapshot (frame 0), so no
				// partial state is presented.
				this.#group = undefined;
				continue;
			}

			if (frame === undefined) {
				// The group is exhausted; advance to the next one.
				this.#group = undefined;
				continue;
			}

			this.#apply(frame.payload);
			return this.#decoder.decode();
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<T> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}

	// Frame 0 of a group is a snapshot, the rest are merge patches.
	#apply(payload: Uint8Array): void {
		if (this.#framesRead === 0) {
			this.#decoder.snapshot(payload);
		} else {
			this.#decoder.delta(payload);
		}
		this.#framesRead += 1;
	}
}
