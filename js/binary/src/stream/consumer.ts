import { Decoder as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";

/** Options for a {@link Consumer}. */
export interface ConsumerConfig {
	/** Whether the frames are `deflate-raw` compressed. Must match the producer. Defaults to `false`. */
	compression?: boolean;
}

/**
 * Thrown by a stream read when the track carried a second group, which a lossless log cannot do.
 *
 * A stream is a single group by construction: a publisher that cannot write a payload ends the
 * track rather than rolling. A second group therefore means whatever would have completed the
 * first is gone, so the read reports it instead of handing back the remainder as a continuous log.
 *
 * Mirrors the Rust `moq_binary::Error::Rolled`.
 */
export class Rolled extends Error {
	constructor() {
		super("rolled: the stream carried a second group, so records are missing");
		this.name = "Rolled";
	}
}

/**
 * Consumes an ordered log of binary payloads from a track, yielding every one in order.
 *
 * The log is a single group. That is what makes the mode lossless: rolling to a second group means
 * the payloads that would have completed the first are gone, so a publisher that cannot write ends
 * the track instead. A second group is therefore a broken publisher, and reading it would present
 * a gap as a continuous log, so it throws {@link Rolled} rather than yielding the remainder.
 *
 * The failure does not wait for the first group to end: whatever has already arrived in it is
 * yielded, and the read then throws rather than blocking on a group a broken publisher may never
 * finish.
 */
export class Consumer {
	#track: Moq.Track.Subscriber;
	#decompress: boolean;

	#group?: Moq.Group.Consumer;
	// Whether the log's one group has been taken, so a second is a rolled log rather than the first.
	#taken = false;
	// Sticky once a second group is seen: the payloads it displaced are gone, so every later read
	// throws too rather than reporting the rest of the log as a whole one.
	#rolled = false;
	// The in-flight `recvGroup`, kept across reads. Racing it against a frame read leaves it
	// outstanding whenever the frame wins, and a second call would take a second group off the track.
	#pending?: Promise<Moq.Group.Consumer | undefined>;
	// The DEFLATE window for the group, present while decompressing.
	#flate?: Flate;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decompress = config.compression ?? false;
	}

	/** Get the next payload, or `undefined` once the track ends. */
	async next(): Promise<Uint8Array | undefined> {
		for (;;) {
			if (this.#rolled) throw new Rolled();

			if (!this.#group) {
				const group = await this.#recvGroup();
				this.#pending = undefined;
				if (!group) return undefined;
				if (this.#taken) {
					this.#rolled = true;
					continue;
				}
				this.#taken = true;
				this.#flate = this.#decompress ? new Flate() : undefined;
				this.#group = group;
			}

			const frame = await this.#readFrame(this.#group);
			if (frame) return this.#flate ? this.#flate.frame(frame.payload) : frame.payload;

			// The log's one group is exhausted. Keep reading the track so a clean end still
			// reports the log as complete, and so a second group is caught as Rolled.
			this.#group = undefined;
		}
	}

	// Read the group's next frame, throwing {@link Rolled} if the track hands over a second group
	// first. A stream is one group, so the track is watched while the group is held: a publisher
	// that opens a second and leaves the first open would otherwise park this read forever on a log
	// that has already lost payloads.
	async #readFrame(group: Moq.Group.Consumer): Promise<Moq.Group.Frame | undefined> {
		// Drain what already arrived before consulting the track, so only a read that would really
		// block depends on which of the two lands first. `skipped` is the evicted prefix that
		// `readFrame` reports as lagged and `tryReadFrame` would hand back across the gap.
		if (!group.skipped) {
			const buffered = group.tryReadFrame();
			if (buffered) return buffered;
		}

		const frame = group.readFrame();
		const winner = await Promise.race([
			frame.then((frame) => ({ frame }) as const),
			this.#recvGroup().then((group) => ({ group }) as const),
		]);
		if ("frame" in winner) return winner.frame;

		this.#pending = undefined;
		if (winner.group) {
			this.#rolled = true;
			throw new Rolled();
		}

		// The track is finished, which does not truncate the group in hand.
		return await frame;
	}

	// Receive the track's next group, reusing the in-flight read a frame may have raced and won.
	//
	// Arrival order rather than sequence order, because there is only ever one group to take and a
	// second one has to be seen whatever its sequence. The monotonic `nextGroup` would drop a late
	// lower sequence, which is the very loss this reports.
	#recvGroup(): Promise<Moq.Group.Consumer | undefined> {
		this.#pending ??= this.#track.recvGroup();
		return this.#pending;
	}

	/** Iterate over every payload in order, until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}
}
