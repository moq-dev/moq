import * as Moq from "@moq/net";

import { Decoder, type Event, type Group } from "./decoder.ts";

/** Window consumer options, including the source track. */
export type ConsumerConfig = { track: Moq.Track.Subscriber };

// A group that ended early rather than failing: the next one restates the window, so a gap is
// something to resync from. Old and Evicted are how the publisher says it dropped a group it was
// still serving, which reads the same here.
const GAPS: Moq.StreamCode[] = [
	Moq.StreamCode.TooFarBehind,
	Moq.StreamCode.Old,
	Moq.StreamCode.Evicted,
	Moq.StreamCode.GroupTooLarge,
];

/**
 * Consumes a sliding window of JSON records from a track, yielding one event per change.
 *
 * A {@link Decoder} that owns its track: it reads groups and turns each group's header into just the changes this reader has not been told about.
 * When something else already owns the track, use the {@link Decoder} directly.
 *
 * Group rolls never surface. A header restating the
 * window yields nothing for records already delivered, so this reads as one continuous stream of
 * {@link Event}s regardless of how the publisher framed them.
 *
 * Each event carries the timestamp of the frame that produced it, absent on an untimed track. The
 * wire has no per-record time, so a record a reader first learns of from a group header (one it
 * joined after, or missed while behind) gets the header frame's time, not its original push time.
 *
 * @public
 */
export class Consumer<T> {
	#track: Moq.Track.Ordered;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;
	#codec?: Group;
	// Timestamp of the frame whose events the decoder is draining.
	#at?: Moq.Time.Timestamp;
	#reading = false;

	constructor(config: ConsumerConfig) {
		this.#track = config.track.ordered();
		this.#decoder = new Decoder();
	}

	/** Absolute index of the oldest record in the window. */
	get offset(): number {
		return this.#decoder.offset;
	}

	/** Get the next event and its frame's timestamp, or `undefined` once the track ends. */
	next(): Promise<Moq.Timed<Event<T>> | undefined> {
		if (this.#reading) throw new Error("multiple calls to next not supported");
		this.#reading = true;
		return this.#read().finally(() => {
			this.#reading = false;
		});
	}

	async #read(): Promise<Moq.Timed<Event<T>> | undefined> {
		for (;;) {
			// Drain what the last frame produced before reading more, so every queued event is that
			// frame's and shares its timestamp.
			const event = this.#decoder.next();
			if (event) return { value: event, at: this.#at };

			if (!this.#group) {
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				this.#codec = this.#decoder.group();
			}

			let frame: Moq.Group.Frame | undefined;
			try {
				frame = await this.#group.readFrame();
			} catch (err) {
				// A locally raised gap carries its code too, so this catches both sides.
				if (!(err instanceof Moq.Error.Stream && GAPS.includes(err.code))) throw err;

				// The next group starts with a checkpoint that accounts for everything missed.
				this.#group = undefined;
				this.#codec = undefined;
				continue;
			}
			if (frame === undefined) {
				// This group is exhausted. Wait for a later one, which restates the window; the stream
				// ends only when the track does.
				this.#group = undefined;
				this.#codec = undefined;
				continue;
			}

			if (!this.#codec) throw new Error("an open MoQ group has a window codec");
			this.#codec.decode(frame.payload);
			this.#at = frame.timestamp;
		}
	}

	/** Iterate over events until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Moq.Timed<Event<T>>> {
		for (;;) {
			const event = await this.next();
			if (event === undefined) return;
			yield event;
		}
	}
}
