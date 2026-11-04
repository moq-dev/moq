/**
 * Datagram tracks: live, unreliable payloads with their own sequence counter.
 *
 * @module
 */
import { type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import { StreamCode, StreamError } from "./error.ts";
import { hooks } from "./internal.ts";
import { rescale } from "./rescale.ts";
import { Timescale, type Timestamp } from "./time.ts";
import { checkSequence, max, UNBOUNDED } from "./util/sequence.ts";

export { rescale } from "./rescale.ts";

/** A single unreliable payload, addressed within its datagram track. */
export interface Datagram {
	/** The datagram track's own sequence number. */
	sequence: bigint;
	/** Presentation timestamp, present exactly when the track has a timescale. */
	timestamp?: Timestamp;
	/** The datagram payload. */
	payload: Uint8Array;
}

/** Publisher properties fixed for the lifetime of a datagram track. */
export interface Info {
	/** Timestamp units per second, or omitted for an untimed track. */
	timescale?: Timescale | null;
	/** Publisher priority, higher first, breaking ties between subscriber priorities. */
	priority: number;
}

// One shared buffer bounds payload retention independently of the number of readers.
const MAX_BUFFERED = 64;
const MAX_BYTES = 64 * 1024;
const MAX_PAYLOAD = 65535;

function priority(value: number): number {
	if (!Number.isInteger(value) || value < -128 || value > 127) throw new RangeError(`invalid priority: ${value}`);
	return value;
}

class State {
	info = new Signal<Info | undefined>(undefined);
	closed = new Once<Error | null>();
	changed = new Signal(0);
	priority = new Signal<number | undefined>(undefined);
	consumers = new Map<object, number>();
	buffer: Datagram[] = [];
	bytes = 0;
	offset = 0n;
	next = 0n;

	release(): void {
		this.offset += BigInt(this.buffer.length);
		this.buffer = [];
		this.bytes = 0;
	}

	end(error?: Error): void {
		if (this.closed.peek() !== undefined) return;
		this.closed.set(error ?? null);
		if (error || this.consumers.size === 0) this.release();
	}

	updatePriority(): void {
		let highest: number | undefined;
		for (const value of this.consumers.values()) highest = Math.max(highest ?? -128, value);
		this.priority.set(highest);
		if (highest === undefined) {
			if (this.info.peek() === undefined) this.end(new StreamError(StreamCode.Cancel));
			if (this.closed.peek() !== undefined) this.release();
		}
	}
}

async function info(state: State, local?: GetPromise<Error | null>): Promise<Info> {
	for (;;) {
		const value = state.info.peek();
		if (value) return value;
		const left = local?.peek();
		if (left !== undefined) throw left ?? new StreamError(StreamCode.Cancel);
		const closed = state.closed.peek();
		if (closed !== undefined) throw closed ?? new Error("datagram track closed before its info");
		await (local !== undefined
			? Signal.race(state.info, state.closed, local)
			: Signal.race(state.info, state.closed));
	}
}

let consume: (name: string, state: State, cursor?: bigint, priority?: number) => Consumer;

/** Writes a live datagram track, independently of any stream track of the same name. */
export class Producer {
	/** The track's name within its broadcast. */
	readonly name: string;
	#state = new State();

	constructor(name: string) {
		this.name = name;
	}

	static {
		hooks.closeUnusedDatagrams = (producer) => {
			if (producer.#state.consumers.size > 0) return false;
			producer.abort(new StreamError(StreamCode.Cancel));
			return true;
		};
	}

	/** Commit the immutable publisher properties, returning this producer. */
	accept(properties: Partial<Info> = {}): this {
		try {
			const state = this.#state;
			if (state.closed.peek() !== undefined) throw state.closed.peek() ?? new Error("datagram track is closed");
			if (state.info.peek() !== undefined) throw new Error("datagram track is already accepted");
			const timescale = properties.timescale == null ? undefined : Timescale(properties.timescale);
			state.info.set(Object.freeze({ timescale, priority: priority(properties.priority ?? 0) }));
			return this;
		} catch (error) {
			this.abort(error instanceof Error ? error : new Error(String(error)));
			throw error;
		}
	}

	/** The track's publisher properties, once accepted. */
	info(): Promise<Info> {
		return info(this.#state);
	}

	/** The highest subscriber priority, or undefined while nobody reads. */
	get priority(): Getter<number | undefined> {
		return this.#state.priority;
	}

	/** The track's terminal result: null for a finish, or its abort error. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A new consumer, starting with the next datagram written. */
	consume(): Consumer {
		return consume(this.name, this.#state);
	}

	/** Append a payload with the next sequence, returning that sequence. */
	append(timestamp: Timestamp | undefined, payload: Uint8Array): bigint {
		const sequence = this.#state.next;
		this.insert({ sequence, timestamp, payload });
		return sequence;
	}

	/** Forward a datagram without renumbering; repeats and older sequences are delivered too. */
	insert(datagram: Datagram): void {
		const state = this.#state;
		const closed = state.closed.peek();
		if (closed !== undefined) throw closed ?? new Error("datagram track is closed");
		const properties = state.info.peek();
		if (!properties) throw new Error("datagram track has not been accepted");
		const sequence = checkSequence(datagram.sequence);
		if (sequence >= UNBOUNDED) throw new RangeError("datagram sequence exhausted");
		if (datagram.payload.byteLength > MAX_PAYLOAD) throw new Error("datagram payload too large");
		const timestamp = rescale(properties, datagram.timestamp);
		// A small view must not pin a peer's larger receive buffer.
		const payload = new Uint8Array(datagram.payload);
		state.next = max(state.next, sequence + 1n);
		state.bytes += payload.byteLength;
		while (state.buffer.length >= MAX_BUFFERED || (state.buffer.length > 0 && state.bytes > MAX_BYTES)) {
			state.bytes -= state.buffer[0].payload.byteLength;
			state.buffer.shift();
			state.offset++;
		}
		state.buffer.push({ sequence, timestamp, payload });
		state.changed.update((revision) => revision + 1);
	}

	/** End cleanly; existing consumers first read their buffered datagrams. */
	finish(): void {
		this.#state.end();
	}

	/** Abort immediately, discarding buffered datagrams. */
	abort(error: Error): void {
		this.#state.end(error);
	}
}

/** A datagram track requested on demand, waiting for its publisher's answer. */
export class Request {
	#producer: Producer;
	#pending: Set<Request>;
	private constructor(producer: Producer, pending: Set<Request>) {
		this.#producer = producer;
		this.#pending = pending;
		pending.add(this);
	}
	static {
		hooks.datagramRequest = (producer, pending) => new Request(producer, pending);
	}
	/** The requested track's name. */
	get name(): string {
		return this.#producer.name;
	}
	/** The highest priority among its waiting consumers. */
	get priority(): Getter<number | undefined> {
		return this.#producer.priority;
	}
	/** Settles when the request or its accepted producer ends. */
	get closed(): GetPromise<Error | null> {
		return this.#producer.closed;
	}
	/** Accept the request and produce datagrams with these properties. */
	accept(properties: Partial<Info> = {}): Producer {
		this.#pending.delete(this);
		return this.#producer.accept(properties);
	}
	/** Refuse the request with the error its consumers receive. */
	reject(error: Error): void {
		this.#pending.delete(this);
		this.#producer.abort(error);
	}
}

/** Reads datagrams in arrival order, live from when this consumer was created. */
export class Consumer {
	/** The track's name within its broadcast. */
	readonly name: string;
	#state: State;
	#cursor: bigint;
	#closed = new Once<Error | null>();
	#priority: number;

	private constructor(name: string, state: State, cursor?: bigint, value = 0) {
		this.name = name;
		this.#state = state;
		this.#cursor = cursor ?? state.offset + BigInt(state.buffer.length);
		this.#priority = value;
		if (state.closed.peek() === undefined || cursor !== undefined) {
			state.consumers.set(this, value);
			state.updatePriority();
		}
	}
	static {
		consume = (name, state, cursor, priority) => new Consumer(name, state, cursor, priority);
	}

	/** The track's publisher properties, once accepted. */
	info(): Promise<Info> {
		return info(this.#state, this.#closed);
	}

	/** This consumer's subscriber priority. */
	get priority(): number {
		return this.#priority;
	}

	/** Change this consumer's priority, higher first. */
	setPriority(value: number): void {
		this.#priority = priority(value);
		if (!this.#state.consumers.has(this)) return;
		this.#state.consumers.set(this, value);
		this.#state.updatePriority();
	}

	/** Another consumer at the same position and priority. */
	clone(): Consumer {
		if (this.#closed.peek() !== undefined) throw new Error("datagram consumer is closed");
		return consume(this.name, this.#state, this.#cursor, this.#priority);
	}

	/** Read the next datagram, skipping any dropped by the bounded shared buffer. */
	async recv(): Promise<Datagram | undefined> {
		for (;;) {
			const local = this.#closed.peek();
			if (local !== undefined) {
				if (local) throw local;
				return;
			}
			const state = this.#state;
			const closed = state.closed.peek();
			if (closed instanceof Error) throw closed;
			this.#cursor = max(this.#cursor, state.offset);
			const datagram = state.buffer[Number(this.#cursor - state.offset)];
			if (datagram) {
				this.#cursor++;
				return datagram;
			}
			if (closed !== undefined) return;
			await Signal.race(state.changed, state.closed, this.#closed);
		}
	}

	/** Release this consumer and wake its pending reads. */
	close(error?: Error): void {
		if (this.#closed.peek() !== undefined) return;
		this.#closed.set(error ?? null);
		this.#state.consumers.delete(this);
		this.#state.updatePriority();
	}
}
