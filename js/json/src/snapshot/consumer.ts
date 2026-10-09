import type { Timed } from "@moq/net";
import * as Moq from "@moq/net";

import { Decoder } from "./decoder.ts";
import type { Config as CodecConfig } from "./encoder.ts";

const GAPS: Moq.StreamCode[] = [
	Moq.StreamCode.TooFarBehind,
	Moq.StreamCode.Old,
	Moq.StreamCode.Evicted,
	Moq.StreamCode.GroupTooLarge,
];

/**
 * Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
 *
 * A {@link Decoder} that owns its track: it reads groups, routes each frame by its position, and
 * yields each reconstructed value with the timestamp of the frame that produced it. Two reads:
 * {@link next} yields every state in order, for a caller that picks the state matching a playhead,
 * and {@link latest} skips to the newest state, for a caller that only wants the current value.
 * When something else already owns the track, use the {@link Decoder} directly.
 */
export class Consumer<T> {
	#track: Moq.Track.Ordered;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;
	#framesRead = 0;

	constructor(config: Consumer.Config<T>) {
		this.#track = config.track.ordered();
		this.#decoder = new Decoder(config);
	}

	/**
	 * Get the next state in order, or `undefined` once the track ends.
	 *
	 * Yields one state per frame and finishes a group before starting the next. A group the
	 * subscription's `maxDelay` proves too old is skipped, and a group lost to a gap resyncs from
	 * the next one, so a reader that falls behind still jumps ahead rather than queueing forever.
	 */
	next(): Promise<Timed<T> | undefined> {
		return this.#read(false);
	}

	/**
	 * Get the newest state, or `undefined` once the track ends.
	 *
	 * Skips to the newest group and applies every frame already buffered in it, yielding only the
	 * last: a late joiner (or any consumer that has fallen behind) catches up to the head in one
	 * step instead of replaying every superseded state. Frames are still decoded in order (the
	 * DEFLATE window and merge patches are sequential); only the per-frame yield is skipped. Blocks
	 * when nothing newer than the last yield has arrived.
	 */
	latest(): Promise<Timed<T> | undefined> {
		return this.#read(true);
	}

	async #read(skip: boolean): Promise<Timed<T> | undefined> {
		for (;;) {
			if (skip) {
				// A newer group supersedes everything the current one still holds: it restarts from a
				// full snapshot, so switching mid-group loses nothing but stale state (mirrors the Rust
				// consumer). Older groups only hold superseded state too, so raise the floor to the
				// newest instead of replaying each one.
				const latest = this.#track.latest();
				if (this.#group !== undefined && latest !== undefined && latest > this.#group.sequence) {
					this.#group.close();
					this.#group = undefined;
				}
				if (!this.#group && latest !== undefined) this.#track.setGroups({ start: { included: latest } });
			}

			if (!this.#group) {
				// Advance to the next group with a higher sequence number (skipping late arrivals).
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				// The next frame is the new group's snapshot, which also restarts the decoder's window.
				this.#framesRead = 0;
			}

			let frame: Moq.Group.Frame | undefined;
			try {
				frame = await this.#group.readFrame();
			} catch (err) {
				if (!(err instanceof Moq.Error.Stream && GAPS.includes(err.code))) throw err;
				// The group was reset or we fell behind its eviction window. Resync from the next
				// group, which begins with a fresh snapshot (frame 0), so no partial state is presented.
				this.#group = undefined;
				continue;
			}

			if (!frame) {
				// The group is exhausted; advance to the next one.
				this.#group = undefined;
				continue;
			}

			this.#apply(frame.payload);
			if (skip) {
				// Apply the rest of the backlog but yield only the head.
				for (let more = this.#group.tryReadFrame(); more; more = this.#group.tryReadFrame()) {
					this.#apply(more.payload);
					frame = more;
				}
			}

			// Every group starts with a snapshot, so a frame has been applied to one by now.
			return { value: this.#decoder.decode() as T, at: frame.timestamp };
		}
	}

	/** Iterate over every state in order, as {@link next} yields them, until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Timed<T>> {
		for (;;) {
			const state = await this.next();
			if (state === undefined) return;
			yield state;
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

export namespace Consumer {
	/** Snapshot consumer options, including the source track. */
	export type Config<T> = Pick<CodecConfig<T>, "schema" | "compression"> & {
		track: Moq.Track.Subscriber;
	};
}
