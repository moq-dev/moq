import { Signal } from "@moq/signals";
import * as Path from "./path.js";

/**
 * The availability of a broadcast.
 *
 * @public
 */
export interface AnnouncedEntry {
	path: Path.Valid;
	active: boolean;
}

/** Reactive backing state for an {@link Announced}: the pending queue plus a closed flag. */
export class AnnouncedState {
	queue = new Signal<AnnouncedEntry[]>([]);
	closed = new Signal<boolean | Error>(false);
}

/**
 * Handles writing announcements to the announcement queue.
 *
 * @public
 */
export class Announced {
	/** Reactive backing state. */
	state = new AnnouncedState();

	/** Path prefix this stream is scoped to. */
	prefix: Path.Valid;

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(prefix = Path.empty()) {
		this.prefix = prefix;
		this.closed = new Promise((resolve) => {
			const dispose = this.state.closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/**
	 * Writes an announcement to the queue.
	 * @param announcement - The announcement to write
	 */
	append(announcement: AnnouncedEntry) {
		if (this.state.closed.peek()) throw new Error("announced is closed");
		this.state.queue.mutate((queue) => {
			queue.push(announcement);
		});
	}

	/**
	 * Closes the writer.
	 * @param abort - If provided, throw this exception instead of returning undefined.
	 */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);
		this.state.queue.mutate((queue) => {
			queue.length = 0;
		});
	}

	/**
	 * Returns the next announcement.
	 */
	async next(): Promise<AnnouncedEntry | undefined> {
		for (;;) {
			const announce = this.state.queue.peek().shift();
			if (announce) return announce;

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.state.queue, this.state.closed);
		}
	}
}
