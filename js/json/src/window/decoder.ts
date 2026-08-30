import { Decoder as Flate } from "@moq/flate";

function object(value: unknown, label: string): Record<string, unknown> {
	if (value === null || typeof value !== "object" || Array.isArray(value)) {
		throw new Error(`${label} must be an object`);
	}
	return value as Record<string, unknown>;
}

function index(value: unknown, label: string): number {
	if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
		throw new Error(`${label} must be a nonnegative safe integer`);
	}
	return value;
}

/** Options for a {@link Decoder}, and so for the {@link Consumer} wrapping one. */
export interface ConsumerConfig {
	/** Read frames written with `ProducerConfig.compression` on. Defaults to `false`. */
	compression?: boolean;
}

/**
 * One change to the window, as the consumer sees it.
 *
 * A record is `push`ed when it first reaches this consumer. Contiguous spans are `pop`ped when they
 * leave the window or `skip`ped when they were dropped before this consumer saw them.
 */
export type Event<T> = { push: { index: number; value: T } } | { pop: Span } | { skip: Span };

/** A half-open range of absolute record indices. */
export interface Span {
	/** First index in the span. */
	start: number;

	/** One past the last index in the span. */
	end: number;
}

/**
 * Reconstructs window events from frame payloads.
 *
 * The track-free core of {@link Consumer}. It tracks indices, not contents: it knows where the
 * window starts and how far it has delivered, which is all it needs to turn a header into the
 * pushes, pops, and skips the reader has not already been told about.
 *
 * Group rolls are invisible here on purpose. A header restates the window, and this decoder emits
 * only what is new, so a reader sees one continuous stream of edits no matter how often the
 * publisher rolled for compression's sake.
 */
export class Decoder<T> {
	#compress: boolean;
	#flate?: Flate;

	// Absolute index of the window's front, once a group header has positioned us.
	#front = 0;
	#len = 0;
	// Next index to deliver, or undefined before the first header. A fresh consumer adopts the first
	// header's offset rather than skipping everything that came before it.
	#delivered?: number;
	// Whether the current group header has been decoded.
	#positioned = false;

	#events: Event<T>[] = [];
	#nextEvent = 0;

	constructor(config: ConsumerConfig = {}) {
		this.#compress = config.compression ?? false;
	}

	/**
	 * Start a cold DEFLATE window, for a reader that has just moved to a new group.
	 *
	 * Only the group-local state resets. The index cursor deliberately survives: it is what lets the
	 * next group's header report just the records this reader has not seen.
	 */
	startGroup(): void {
		this.#flate = undefined;
		this.#positioned = false;
	}

	/** Absolute index of the oldest record in the window. */
	get offset(): number {
		return this.#front;
	}

	/** Take the next event produced by the frames decoded so far. */
	next(): Event<T> | undefined {
		const event = this.#events[this.#nextEvent++];
		if (this.#nextEvent >= this.#events.length) {
			this.#events = [];
			this.#nextEvent = 0;
		}
		return event;
	}

	/** Decode one frame, queueing the events it implies. */
	decode(payload: Uint8Array): void {
		if (this.#compress) this.#flate ??= new Flate();
		const bytes = this.#flate ? this.#flate.frame(payload) : payload;
		const frame = object(JSON.parse(new TextDecoder().decode(bytes)) as unknown, "window frame");

		if (!this.#positioned) {
			if (!Array.isArray(frame.records)) throw new Error("window header records must be an array");
			this.#applyHeader(index(frame.offset, "window offset"), frame.records as T[]);
			return;
		}

		const keys = Object.keys(frame);
		if (keys.length !== 1) throw new Error("window op must contain exactly one operation");

		switch (keys[0]) {
			case "push":
				this.#applyPush(frame.push as T);
				break;
			case "pop":
				this.#applyPop(index(frame.pop, "window pop count"));
				break;
			default:
				throw new Error("unrecognized window op");
		}
	}

	/** The window is exactly these records. Report what this reader missed, then what is new. */
	#applyHeader(offset: number, records: T[]): void {
		const end = offset + records.length;
		if (!Number.isSafeInteger(end)) throw new Error("window range exceeds the safe integer range");
		let delivered = this.#delivered;

		if (delivered === undefined) {
			// First position: adopt the publisher's offset rather than skipping all of history.
			delivered = offset;
		} else {
			if (offset < this.#front || end < delivered) throw new Error("window header moved backwards");

			// Records that left the window while we were away. Those we had delivered are pops; those we
			// never saw are skips. Keep each gap compact: the offset is untrusted and may jump by far
			// more indices than a consumer could materialize individually.
			this.#range("pop", this.#front, Math.min(delivered, offset));
			this.#range("skip", delivered, offset);
		}

		// Deliver only the tail this reader has not seen; a header that merely restates what it holds
		// yields nothing at all.
		for (let index = Math.max(delivered, offset); index < end; index++) {
			this.#events.push({ push: { index, value: records[index - offset] as T } });
		}

		this.#front = offset;
		this.#len = end - offset;
		this.#delivered = Math.max(delivered, end);
		this.#positioned = true;
	}

	/** One record joined the back. */
	#applyPush(value: T): void {
		if (this.#delivered === undefined) throw new Error("window op before group header");

		const index = this.#front + this.#len;
		if (!Number.isSafeInteger(index + 1)) throw new Error("window range exceeds the safe integer range");
		this.#len += 1;

		if (index >= this.#delivered) {
			this.#events.push({ push: { index, value } });
			this.#delivered = index + 1;
		}
	}

	/** Records left the front. */
	#applyPop(count: number): void {
		if (this.#delivered === undefined) throw new Error("window op before group header");
		if (count > this.#len) {
			throw new Error(`pop of ${count} exceeds the ${this.#len} record(s) in the window`);
		}

		const end = this.#front + count;
		this.#range("pop", this.#front, Math.min(this.#delivered, end));
		this.#range("skip", Math.max(this.#delivered, this.#front), end);

		this.#front = end;
		this.#len -= count;
		this.#delivered = Math.max(this.#delivered, this.#front);
	}

	#range(kind: "pop" | "skip", start: number, end: number): void {
		if (start >= end) return;
		this.#events.push(kind === "pop" ? { pop: { start, end } } : { skip: { start, end } });
	}
}
