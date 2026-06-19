import { Signal } from "@moq/signals";
import { Track } from "./track.ts";

/** A track a subscriber asked for, along with its requested delivery priority. */
export interface TrackRequest {
	track: Track;
	priority: number;
}

/** Reactive backing state for a {@link Broadcast}: requested tracks plus a closed flag. */
export class BroadcastState {
	requested = new Signal<TrackRequest[]>([]);
	closed = new Signal<boolean | Error>(false);
}

/**
 * Handles writing and managing tracks in a broadcast.
 *
 * @public
 */
export class Broadcast {
	/** Reactive backing state. */
	state = new BroadcastState();

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor() {
		this.closed = new Promise((resolve) => {
			const dispose = this.state.closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/**
	 * A track requested over the network.
	 */
	async requested(): Promise<TrackRequest | undefined> {
		for (;;) {
			// We use pop instead of shift because it's slightly more efficient.
			const track = this.state.requested.peek().pop();
			if (track) return track;

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.state.requested, this.state.closed);
		}
	}

	/**
	 * Populates the provided track over the network.
	 */
	subscribe(name: string, priority: number): Track {
		const track = new Track(name);

		if (this.state.closed.peek()) {
			throw new Error(`broadcast is closed: ${this.state.closed.peek()}`);
		}
		this.state.requested.mutate((requested) => {
			requested.push({ track, priority });
			// Sort the tracks by priority in ascending order (we will pop)
			requested.sort((a, b) => a.priority - b.priority);
		});

		return track;
	}

	/**
	 * Closes the writer and all associated tracks.
	 *
	 * @param abort - If provided, throw this exception instead of returning undefined.
	 */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);
		for (const { track } of this.state.requested.peek()) {
			track.close(abort);
		}
		this.state.requested.mutate((requested) => {
			requested.length = 0;
		});
	}
}
