// Hand-written TypeScript shim over the wasm-bindgen output (`../dist/moq.js`).
//
// The generated classes are primitive (frames are `Uint8Array`, options are
// positional, `sequence` is a `bigint`). This layer wraps them to present the
// exact `@moq/net` surface so it can be a drop-in replacement: the
// string/json/bool conveniences, options-object signatures, the `Connection`
// namespace, a reactive `state.closed` signal, lazy `consume`, and `number`
// sequences.
//
// The pure helpers (`Path`, `Time`) and the `TrackInfo` type are re-exported
// from `@moq/net` rather than copied: they carry no wire code, so with
// `@moq/net`'s `sideEffects: false` a bundler drops everything else, and the
// branded types stay identical to the ones publish/watch already import. (A
// future `@moq/model` package would hold these and let `@moq/wasm` stand alone.)

import { Path, Time, type TrackInfo } from "@moq/net";
import { Signal } from "@moq/signals";
import initWasm, * as Wasm from "#bindgen";

export type { AnnouncedEntry } from "@moq/net";
// Pure, wire-free helpers re-exported from @moq/net so the barrel matches it
// (tree-shaken via @moq/net's `sideEffects: false`; identical branded types).
export { Signals, Varint } from "@moq/net";
export * as Connection from "./connection.ts";
export type { TrackInfo };
export { Path, Time };

// Load the wasm module once. `--target web` fetches `moq_bg.wasm` relative to
// the JS via `import.meta.url`, which bundlers (vite/esbuild) resolve as an asset.
let loaded: Promise<void> | undefined;

/**
 * Load the wasm module and install the panic/tracing hooks.
 *
 * `filter` is a RUST_LOG-style tracing directive (e.g. `"moq_net=debug"`,
 * `"warn,moq_net::lite=trace,wasm=trace"`). When omitted it falls back to
 * `localStorage.moq_log`, so you can crank up logging from the browser console
 * (`localStorage.moq_log = "moq_net::lite=trace"`) and reload without a rebuild.
 * Defaults to `"warn"` inside the wasm if neither is set.
 */
export function init(filter?: string): Promise<void> {
	if (!loaded) {
		const directive = filter ?? logDirective();
		loaded = initWasm().then(() => {
			Wasm.setup(directive);
		});
	}
	return loaded;
}

function logDirective(): string | undefined {
	try {
		return globalThis.localStorage?.getItem("moq_log") ?? undefined;
	} catch {
		// localStorage can throw (private mode, no DOM); fall back to the default.
		return undefined;
	}
}

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

/**
 * A request for a track the peer wants, yielded by {@link Broadcast.requested}.
 * Mirrors `@moq/net`'s `TrackRequest`.
 */
export class TrackRequest {
	#inner: Wasm.TrackRequest;
	readonly name: string;
	readonly priority: number;

	constructor(inner: Wasm.TrackRequest) {
		this.#inner = inner;
		this.name = inner.name;
		this.priority = inner.priority;
	}

	/** Accept the request, committing the immutable {@link TrackInfo} and returning a producer. */
	accept(info: Partial<TrackInfo> = {}): TrackProducer {
		return new TrackProducer(this.#inner.accept(info));
	}

	/** Reject the request, closing the track. */
	reject(_err?: Error): void {
		this.#inner.reject();
	}
}

/** A lazy handle to a track on a consumed broadcast. Mirrors `@moq/net`'s `TrackConsumer`. */
export class TrackConsumer {
	// Resolves the underlying broadcast (which may still be waiting for its announce).
	#broadcast: () => Promise<Wasm.Broadcast | undefined>;
	readonly name: string;

	constructor(broadcast: () => Promise<Wasm.Broadcast | undefined>, name: string) {
		this.#broadcast = broadcast;
		this.name = name;
	}

	async #track(): Promise<Wasm.TrackConsumer> {
		const broadcast = await this.#broadcast();
		if (!broadcast) throw new Error(`broadcast not found for track ${this.name}`);
		return broadcast.track(this.name);
	}

	/**
	 * Open a live subscription, streaming the track's groups. `priority` defaults
	 * to `0`. Returns synchronously (like `@moq/net`); the wire subscribe resolves
	 * in the background and the reader methods await it.
	 */
	subscribe(options?: { priority?: number }): TrackSubscriber {
		const priority = options?.priority ?? 0;
		const inner = this.#track().then((track) => track.subscribe(priority));
		return new TrackSubscriber(this.name, inner);
	}

	/** Fetch the track's immutable publisher properties without subscribing (lite-05+). */
	async info(): Promise<TrackInfo> {
		const track = await this.#track();
		return (await track.info()) as TrackInfo;
	}
}

/** Reactive `state.closed`, the one piece of `@moq/net`'s model the apps read directly. */
class ClosedState {
	readonly closed = new Signal<boolean | Error>(false);
}

/** The write side of a track. Mirrors `@moq/net`'s `TrackProducer`. */
export class TrackProducer {
	#inner: Wasm.TrackProducer;
	readonly name: string;
	readonly state = new ClosedState();
	readonly closed: Promise<Error | undefined>;

	constructor(inner: Wasm.TrackProducer) {
		this.#inner = inner;
		this.name = inner.name;

		this.closed = inner.closed().then((msg) => {
			const err = msg ? new Error(msg) : undefined;
			this.state.closed.set(err ?? true);
			return err;
		});
	}

	/** Append a new group with the next sequence number. */
	appendGroup(): Group {
		return Group.fromWasm(this.#inner.appendGroup());
	}

	/** Append a frame as its own single-frame group. */
	writeFrame(frame: Uint8Array): void {
		this.#inner.writeFrame(frame);
	}

	writeString(str: string): void {
		this.writeFrame(textEncoder.encode(str));
	}

	writeJson(json: unknown): void {
		this.writeString(JSON.stringify(json));
	}

	writeBool(bool: boolean): void {
		this.writeFrame(new Uint8Array([bool ? 1 : 0]));
	}

	/** Close the track (cleanly when no error, aborting otherwise). */
	close(abort?: Error): void {
		if (abort) this.#inner.abort(abort.message);
		else this.#inner.close();
		this.state.closed.set(abort ?? true);
	}
}

/** The read side of a live track subscription. Mirrors `@moq/net`'s `TrackSubscriber`. */
export class TrackSubscriber {
	// The wire subscribe resolves in the background; readers await it. This keeps
	// `subscribe()` synchronous, matching `@moq/net`.
	#inner: Promise<Wasm.TrackSubscriber>;
	#nextSequence = 0;
	readonly name: string;

	constructor(name: string, inner: Promise<Wasm.TrackSubscriber>) {
		this.name = name;
		this.#inner = inner;
	}

	async info(): Promise<TrackInfo> {
		return (await this.#inner).info() as TrackInfo;
	}

	/** Receive the next group in arrival order, or `undefined` when the track ends. */
	async recvGroup(): Promise<Group | undefined> {
		const group = await (await this.#inner).recvGroup();
		return group ? Group.fromWasm(group) : undefined;
	}

	/** Next group with a strictly-greater sequence than the last returned (skips late arrivals). */
	async nextGroup(): Promise<Group | undefined> {
		const inner = await this.#inner;
		for (;;) {
			const group = await inner.nextGroup();
			if (!group) return undefined;
			const wrapped = Group.fromWasm(group);
			if (wrapped.sequence < this.#nextSequence) {
				wrapped.close();
				continue;
			}
			this.#nextSequence = wrapped.sequence + 1;
			return wrapped;
		}
	}

	async readFrame(): Promise<Uint8Array | undefined> {
		const group = await this.recvGroup();
		if (!group) return undefined;
		const frame = await group.readFrame();
		group.close();
		return frame;
	}

	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? textDecoder.decode(frame) : undefined;
	}

	async readJson(): Promise<unknown | undefined> {
		const str = await this.readString();
		return str ? JSON.parse(str) : undefined;
	}

	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame[0] === 1 : undefined;
	}

	updatePriority(priority: number): void {
		void this.#inner.then((s) => s.updatePriority(priority));
	}

	/** Stop the subscription (unsubscribes once the wire subscribe resolves). */
	close(_abort?: Error): void {
		void this.#inner.then((s) => s.close());
	}
}

/** A group of frames: writable when produced, readable when consumed. Mirrors `@moq/net`'s `Group`. */
export class Group {
	#inner: Wasm.Group;
	readonly sequence: number;

	private constructor(inner: Wasm.Group) {
		this.#inner = inner;
		this.sequence = Number(inner.sequence);
	}

	static fromWasm(inner: Wasm.Group): Group {
		return new Group(inner);
	}

	writeFrame(frame: Uint8Array): void {
		this.#inner.writeFrame(frame);
	}

	writeString(str: string): void {
		this.writeFrame(textEncoder.encode(str));
	}

	writeJson(json: unknown): void {
		this.writeString(JSON.stringify(json));
	}

	writeBool(bool: boolean): void {
		this.writeFrame(new Uint8Array([bool ? 1 : 0]));
	}

	async readFrame(): Promise<Uint8Array | undefined> {
		return (await this.#inner.readFrame()) ?? undefined;
	}

	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? textDecoder.decode(frame) : undefined;
	}

	async readJson(): Promise<unknown | undefined> {
		const str = await this.readString();
		return str ? JSON.parse(str) : undefined;
	}

	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame[0] === 1 : undefined;
	}

	close(_abort?: Error): void {
		this.#inner.close();
	}
}

/**
 * A broadcast: construct one to publish (`new Broadcast()`), or receive one from
 * {@link Session.consume}. Mirrors `@moq/net`'s dual-use `Broadcast`.
 */
export class Broadcast {
	// Producer side: a concrete wasm handle. Consume side: a (memoized) resolver
	// that waits for the broadcast's announce on first use, keeping `consume` sync.
	readonly #producer?: Wasm.Broadcast;
	readonly #resolve?: () => Promise<Wasm.Broadcast | undefined>;
	#resolved?: Promise<Wasm.Broadcast | undefined>;

	constructor(resolve?: () => Promise<Wasm.Broadcast | undefined>) {
		if (resolve) this.#resolve = resolve;
		else this.#producer = new Wasm.Broadcast();
	}

	// The wasm handle for Session.publish; only meaningful on a producer broadcast.
	get handle(): Wasm.Broadcast | undefined {
		return this.#producer;
	}

	#broadcast(): Promise<Wasm.Broadcast | undefined> {
		if (this.#producer) return Promise.resolve(this.#producer);
		if (!this.#resolved) this.#resolved = this.#resolve?.() ?? Promise.resolve(undefined);
		return this.#resolved;
	}

	/** A track requested over the network (producer side), or `undefined` once closed. */
	async requested(): Promise<TrackRequest | undefined> {
		if (!this.#producer) throw new Error("requested() is only valid on a published broadcast");
		const request = await this.#producer.requested();
		return request ? new TrackRequest(request) : undefined;
	}

	/** Get a lazy {@link TrackConsumer} handle for a track (consumer side). */
	track(name: string): TrackConsumer {
		return new TrackConsumer(() => this.#broadcast(), name);
	}

	/** Open a live subscription to a track (consumer side). */
	subscribe(name: string, priority = 0): TrackSubscriber {
		return this.track(name).subscribe({ priority });
	}

	close(_abort?: Error): void {
		this.#producer?.close();
		void this.#resolved?.then((b) => b?.close());
	}
}

/** A live announce / unannounce stream. Mirrors `@moq/net`'s `Announced`. */
export class Announced {
	#inner: Wasm.Announced;

	constructor(inner: Wasm.Announced) {
		this.#inner = inner;
	}

	/** The next `{ path, active }` event, or `undefined` once the stream ends. */
	async next(): Promise<{ path: Path.Valid; active: boolean } | undefined> {
		const entry = (await this.#inner.next()) as { path: string; active: boolean } | undefined;
		if (!entry) return undefined;
		return { path: entry.path as Path.Valid, active: entry.active };
	}

	close(): void {
		this.#inner.close();
	}
}

/** A connected session, presented as a {@link Connection.Established}. */
export class Session {
	readonly url: URL;
	readonly version: string;
	readonly closed: Promise<void>;

	// Bandwidth / RTT telemetry isn't surfaced from wasm yet (always undefined);
	// declared for `@moq/net` `Established` parity.
	readonly sendBandwidth?: Signal<number | undefined>;
	readonly recvBandwidth?: Signal<number | undefined>;
	readonly rtt?: Signal<Time.Milli | undefined>;

	#inner: Wasm.Session;
	// The OriginConsumer carries announce discovery + consume (mirrors moq-net).
	#consumer: Wasm.OriginConsumer;

	constructor(url: URL, inner: Wasm.Session) {
		this.url = url;
		this.#inner = inner;
		this.version = inner.version();
		this.closed = inner.closed();
		this.#consumer = inner.consumer();
	}

	/** A lazy {@link Broadcast} handle; subscribing waits for its announce. */
	consume(broadcast: string): Broadcast {
		return new Broadcast(() => this.#consumer.consume(broadcast));
	}

	/**
	 * Stream announce / unannounce events. The OriginConsumer is already scoped,
	 * so `prefix` is accepted for `@moq/net` parity but not used as a filter.
	 */
	announced(_prefix?: Path.Valid): Announced {
		return new Announced(this.#consumer.announced());
	}

	publish(path: string, broadcast: Broadcast): void {
		const handle = broadcast.handle;
		if (!handle) throw new Error("can only publish a broadcast created with new Broadcast()");
		this.#inner.publish(path, handle);
	}

	close(): void {
		this.#inner.close();
	}
}
