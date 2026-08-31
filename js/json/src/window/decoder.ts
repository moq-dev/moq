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

type Queued<T> = Event<T> | { offset: number; records: T[]; next: number };

/** A half-open range of absolute record indices. */
export interface Span {
	/** First index in the span. */
	start: number;

	/** One past the last index in the span. */
	end: number;
}

/** Decodes the frames in one MoQ group, requiring a header at frame zero. */
export interface Group {
	/** Decode the next frame in this group. */
	decode(payload: Uint8Array): void;
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
 *
 * @public
 */
export class Decoder<T> {
	#compress: boolean;
	#generation = 0;

	// Absolute index of the window's front, once a group header has positioned us.
	#front = 0;
	#len = 0;
	// Next index to deliver, or undefined before the first header. A fresh consumer adopts the first
	// header's offset rather than skipping everything that came before it.
	#delivered?: number;
	#events: Queued<T>[] = [];
	#nextEvent = 0;

	constructor(config: ConsumerConfig = {}) {
		this.#compress = config.compression ?? false;
	}

	/** Create the decoder for one MoQ group. */
	group(): Group {
		const generation = ++this.#generation;
		let flate: Flate | undefined;
		let positioned = false;

		return {
			decode: (payload) => {
				if (generation !== this.#generation) throw new Error("stale window group");
				if (this.#compress) flate ??= new Flate();
				const bytes = flate ? flate.frame(payload) : payload;
				const frame = object(JSON.parse(new TextDecoder().decode(bytes)) as unknown, "window frame");

				if (!positioned) {
					if (!Array.isArray(frame.records)) throw new Error("window header records must be an array");
					this.#applyHeader(index(frame.offset, "window offset"), frame.records as T[]);
					positioned = true;
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
			},
		};
	}

	/** Absolute index of the oldest record in the window. */
	get offset(): number {
		return this.#front;
	}

	/** Take the next event produced by the frames decoded so far. */
	next(): Event<T> | undefined {
		const queued = this.#events[this.#nextEvent];
		if (!queued) {
			this.#clearEvents();
			return undefined;
		}

		if ("records" in queued) {
			const next = queued.next++;
			const event = { push: { index: queued.offset + next, value: queued.records[next] as T } };
			if (queued.next >= queued.records.length) this.#advanceEvent();
			return event;
		}

		this.#advanceEvent();
		return queued;
	}

	#advanceEvent(): void {
		this.#nextEvent += 1;
		if (this.#nextEvent >= this.#events.length) this.#clearEvents();
	}

	#clearEvents(): void {
		this.#events = [];
		this.#nextEvent = 0;
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

		// Keep the unseen tail as one batch and materialize each push only when the caller asks for it.
		const next = Math.max(delivered - offset, 0);
		if (next < records.length) {
			this.#events.push({ offset, records, next });
		}

		this.#front = offset;
		this.#len = end - offset;
		this.#delivered = Math.max(delivered, end);
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
