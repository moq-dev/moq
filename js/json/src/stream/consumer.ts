import type * as Moq from "@moq/net";

import { type ConsumerConfig, Decoder } from "./decoder.ts";

/**
 * Thrown by a stream read when the track carried a second group, which a lossless log cannot do.
 *
 * A stream is a single group by construction: a publisher that cannot write a record ends the
 * track rather than rolling. A second group therefore means whatever would have completed the
 * first is gone, so the read reports it instead of handing back the remainder as a continuous log.
 *
 * Mirrors the Rust `moq_json::Error::Rolled`.
 */
export class Rolled extends Error {
	constructor() {
		super("rolled: the stream carried a second group, so records are missing");
		this.name = "Rolled";
	}
}

/**
 * Consumes an ordered log of JSON records from a track, yielding every record in order.
 *
 * A {@link Decoder} that owns its track, reading one record per frame. The log is a single group,
 * which is what makes the mode lossless: rolling to a second group means the records that would
 * have completed the first are gone, so a {@link Producer} that cannot write ends the track
 * instead. A second group is therefore a broken publisher, and reading it would present a gap as a
 * continuous log, so it throws {@link Rolled} rather than yielding the remainder. When something
 * else already owns the track, use the {@link Decoder} directly.
 *
 * The failure does not wait for the first group to end: whatever has already arrived in it is
 * yielded, and the read then throws rather than blocking on a group a broken publisher may never
 * finish.
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;
	// Whether a read is in flight, so a second concurrent one is refused rather than served wrong.
	#reading = false;
	// Whether the log's one group has been taken, so a second is a rolled log rather than the first.
	#taken = false;
	// The in-flight `recvGroup`, tagged for the race and built once: a frame read that wins leaves it
	// outstanding, a second call would take a second group off the track, and tagging it per read
	// would chain a reaction onto it for every record in the log. Cleared only when the group it
	// carries is taken, so a second group stays resolved here and every later read fails on it again.
	#pending?: Promise<{ group: Moq.Group.Consumer | undefined }>;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decoder = new Decoder(config);
	}

	/** Get the next record, or `undefined` once the track ends. */
	next(): Promise<T | undefined> {
		// One reader at a time. Two concurrent calls await the same `recvGroup`, so the second would
		// take the first's group for a rolled log and fail a perfectly good one.
		if (this.#reading) throw new Error("multiple calls to next not supported");
		this.#reading = true;
		return this.#read().finally(() => {
			this.#reading = false;
		});
	}

	async #read(): Promise<T | undefined> {
		for (;;) {
			if (!this.#group) {
				const { group } = await this.#recvGroup();
				if (!group) return undefined;
				// The resolved `#pending` is the whole record of the failure: it is left in place, so
				// every later read arrives back here and throws again rather than reading on.
				if (this.#taken) {
					group.close();
					throw new Rolled();
				}
				this.#pending = undefined;
				this.#taken = true;
				// Each group is its own compressed stream, so the window starts cold.
				this.#decoder.reset();
				this.#group = group;
			}

			const frame = await this.#readFrame(this.#group);
			if (frame) return this.#decoder.decode(frame.payload);

			// The log's one group is exhausted. Keep reading the track so a clean end still
			// reports the log as complete, and so a second group is caught as Rolled.
			this.#group = undefined;
		}
	}

	// Read the group's next frame, throwing {@link Rolled} if the track hands over a second group
	// first. A stream is one group, so the track is watched while the group is held: a publisher
	// that opens a second and leaves the first open would otherwise park this read forever on a log
	// that has already lost records.
	async #readFrame(group: Moq.Group.Consumer): Promise<Moq.Group.Frame | undefined> {
		// Drain what already arrived before consulting the track, so only a read that would really
		// block depends on which of the two lands first. `skipped` is the evicted prefix that
		// `readFrame` reports as lagged and `tryReadFrame` would hand back across the gap.
		if (!group.skipped) {
			const buffered = group.tryReadFrame();
			if (buffered) return buffered;
		}

		const frame = group.readFrame();
		const winner = await Promise.race([frame.then((frame) => ({ frame }) as const), this.#recvGroup()]);
		if ("frame" in winner) return winner.frame;

		if (winner.group) {
			// Close both rather than drain the rest of a log that has already lost records. Closing the
			// held group settles the read that just lost, which otherwise stays registered on the
			// group's signals and keeps this consumer's subscription reachable after the caller drops
			// it. The next read then waits on the track, where the second group is still resolved.
			group.close();
			winner.group.close();
			this.#group = undefined;
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
	#recvGroup(): Promise<{ group: Moq.Group.Consumer | undefined }> {
		this.#pending ??= this.#track.recvGroup().then((group) => ({ group }));
		return this.#pending;
	}

	async *[Symbol.asyncIterator](): AsyncIterator<T> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}
}
