import { Signal } from "@moq/signals";
import { Track, type TrackProps } from "./track.js";

/**
 * Handles writing and managing tracks in a broadcast.
 *
 * @public
 */
export class Broadcast {
	#requested = new Signal<Track[]>([]);
	#closed = new Signal<boolean | Error>(false);

	readonly closed: Promise<Error | undefined>;

	constructor() {
		this.closed = new Promise((resolve) => {
			const dispose = this.#closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/**
	 * A track requested over the network.
	 */
	async requested(): Promise<Track | undefined> {
		for (;;) {
			// We use pop instead of shift because it's slightly more efficient.
			const track = this.#requested.peek().pop();
			if (track) return track;

			const closed = this.#closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.#requested, this.#closed);
		}
	}

	/**
	 * Creates a new track and serves it over the network.
	 */
	subscribe(track: TrackProps): Track {
		const t = new Track(track);
		this.serve(t);
		return t;
	}

	/**
	 * Populates the provided track over the network.
	 */
	serve(track: Track) {
		if (this.#closed.peek()) {
			throw new Error(`broadcast is closed: ${this.#closed.peek()}`);
		}
		this.#requested.mutate((requested) => {
			requested.push(track);
		});

		return track;
	}

	/**
	 * Closes the writer and all associated tracks.
	 *
	 * @param abort - If provided, throw this exception instead of returning undefined.
	 */
	close(abort?: Error) {
		this.#closed.set(abort ?? true);
		for (const track of this.#requested.peek()) {
			track.close(abort);
		}
		this.#requested.mutate((requested) => {
			requested.length = 0;
		});
	}
}
