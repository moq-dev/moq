import type { Effect } from "@moq/signals";

// How many values a reader may fall behind before it starts losing its oldest.
const QUEUE = 4;

/** Constructor options for {@link Fanout}. */
export interface FanoutProps<T> {
	/** How many values each reader may buffer before its oldest is dropped. Defaults to 4. */
	queue?: number;

	/**
	 * Produce a reader's own copy of a value.
	 *
	 * Required for values that own a resource (a `VideoFrame`), since each reader closes what it
	 * receives. Omit it for plain immutable data, which every reader can share.
	 */
	clone?: (value: T) => T;

	/** Release a value the fanout is discarding: dropped from a full queue, or never delivered. */
	release?: (value: T) => void;
}

/**
 * Distributes one stream to any number of readers.
 *
 * Each reader gets its own bounded queue, so one that falls behind loses its own oldest values
 * rather than stalling the source or the other readers. Head-of-line blocking would be worse than
 * dropping here: the source is a live capture that can't be asked to wait.
 *
 * This exists because a `Signal` is the wrong channel for media. Signal writes coalesce within a
 * microtask, so a burst delivers only the newest value and the rest are destroyed without any
 * reader seeing them, which for an encoder means silently skipped frames.
 */
export class Fanout<T> {
	readonly #queue: number;
	readonly #clone: ((value: T) => T) | undefined;
	readonly #release: ((value: T) => void) | undefined;

	readonly #readers = new Set<Reader<T>>();

	#closed = false;

	constructor(source: ReadableStream<T>, props?: FanoutProps<T>) {
		this.#queue = props?.queue ?? QUEUE;
		this.#clone = props?.clone;
		this.#release = props?.release;

		void this.#pump(source).catch((err) => console.error("fanout source failed:", err));
	}

	/**
	 * A stream of everything published from now on, cancelled when `effect` is torn down.
	 *
	 * `queue` overrides how far this reader may fall behind before it loses its oldest. Pass 1 for a
	 * consumer that only wants the newest, like a preview that draws one frame per paint.
	 */
	subscribe(effect: Effect, queue = this.#queue): ReadableStream<T> {
		const reader: Reader<T> = { queue: [], limit: queue, waiting: undefined, done: this.#closed };
		this.#readers.add(reader);

		effect.cleanup(() => {
			this.#readers.delete(reader);
			this.#drain(reader);
			reader.done = true;
			reader.waiting?.();
			reader.waiting = undefined;
		});

		return new ReadableStream<T>(
			{
				pull: async (controller) => {
					for (;;) {
						const value = reader.queue.shift();
						if (value !== undefined) {
							controller.enqueue(value);
							return;
						}

						if (reader.done) {
							controller.close();
							return;
						}

						await new Promise<void>((resolve) => {
							reader.waiting = resolve;
						});
					}
				},
				cancel: () => {
					this.#readers.delete(reader);
					this.#drain(reader);
				},
			},
			// A stream buffers ahead by default, which would pull values out of the queue above and
			// hold them where the drop policy can't reach: the reader would keep a stale value and
			// lose a fresh one instead. Pulling only on demand keeps that queue the single buffer.
			{ highWaterMark: 0 },
		);
	}

	/** Stop distributing and release anything still queued. */
	close(): void {
		this.#closed = true;

		for (const reader of this.#readers) {
			this.#drain(reader);
			reader.done = true;
			reader.waiting?.();
			reader.waiting = undefined;
		}

		this.#readers.clear();
	}

	async #pump(source: ReadableStream<T>): Promise<void> {
		const reader = source.getReader();

		for (;;) {
			const { value } = await reader.read();
			if (value === undefined) break;

			// Nobody attached, so this value has nowhere to go.
			if (this.#readers.size === 0) {
				this.#release?.(value);
				continue;
			}

			for (const target of this.#readers) {
				// The last reader can take the original; everyone before it needs its own copy.
				this.#push(target, this.#clone ? this.#clone(value) : value);
			}

			// Every reader holds a clone of its own, so the original is ours to release.
			if (this.#clone) this.#release?.(value);
		}

		this.close();
	}

	#push(reader: Reader<T>, value: T): void {
		// Drop this reader's oldest rather than stalling the source for everyone.
		while (reader.queue.length >= reader.limit) {
			const dropped = reader.queue.shift();
			if (dropped !== undefined) this.#release?.(dropped);
		}

		reader.queue.push(value);

		reader.waiting?.();
		reader.waiting = undefined;
	}

	#drain(reader: Reader<T>): void {
		for (const value of reader.queue) this.#release?.(value);
		reader.queue.length = 0;
	}
}

// One attached reader: what it hasn't consumed yet, plus a resolver parked on an empty queue.
type Reader<T> = {
	queue: T[];
	limit: number;
	waiting: (() => void) | undefined;
	done: boolean;
};
