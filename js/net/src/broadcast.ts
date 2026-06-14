import { Signal } from "@moq/signals";
import { type TrackInfo, TrackProducer, TrackState, TrackSubscriber } from "./track.ts";

/**
 * A request for a track the peer wants, yielded by {@link Broadcast.requested}.
 *
 * The producer answers it with {@link accept}, declaring the track's immutable
 * publisher properties and receiving a {@link TrackProducer} to write groups into,
 * or {@link reject} to refuse it. The properties must be declared up front so the
 * wire layer can answer a TRACK request (lite-05+) and pick the frame codec before
 * any group is served. Mirrors the Rust `TrackRequest`.
 */
export class TrackRequest {
	readonly name: string;
	readonly priority: number;
	// The state behind the TrackSubscriber handed to the caller of Broadcast.subscribe;
	// accepting adopts it as the producer's first sink, which the producer fans into.
	#state: TrackState;

	constructor(name: string, state: TrackState, priority: number) {
		this.name = name;
		this.#state = state;
		this.priority = priority;
	}

	/**
	 * Accept the request, committing the track's immutable {@link TrackInfo} and
	 * returning a {@link TrackProducer} to write groups into. Any field left unset
	 * keeps its default. Mirrors the Rust `TrackRequest::accept`.
	 */
	accept(info: Partial<TrackInfo> = {}): TrackProducer {
		// The producer adopts the already-handed-out subscriber state as its first
		// sink, then commits the info (which propagates to that sink).
		return new TrackProducer(this.name, this.#state).accept(info);
	}

	/** Reject the request, closing the track (optionally with an error). */
	reject(err?: Error): void {
		this.#state.closed.set(err ?? true);
	}
}

/**
 * A lazy handle to a track on a consumed broadcast, mirroring the Rust
 * `TrackConsumer`. Holding it sends nothing over the network; call {@link subscribe}
 * to open a live subscription or {@link info} to fetch the immutable publisher
 * properties (lite-05+).
 */
export class TrackConsumer {
	readonly name: string;
	#broadcast: Broadcast;

	constructor(broadcast: Broadcast, name: string) {
		this.#broadcast = broadcast;
		this.name = name;
	}

	/**
	 * Open a live subscription, returning a {@link TrackSubscriber} streaming the
	 * track's groups. `priority` defaults to `0`.
	 */
	subscribe(options?: { priority?: number }): TrackSubscriber {
		return this.#broadcast.subscribe(this.name, options?.priority ?? 0);
	}

	/**
	 * Fetch the track's immutable publisher properties without subscribing, via a
	 * TRACK stream. Lite-05+ only; rejects on older drafts (which carry no TRACK
	 * stream) and if the track does not exist.
	 */
	info(): Promise<TrackInfo> {
		return this.#broadcast.resolveTrackInfo(this.name);
	}
}

/** Reactive backing state for a {@link Broadcast}: requested tracks plus a closed flag. */
export class BroadcastState {
	requested = new Signal<TrackRequest[]>([]);
	closed = new Signal<boolean | Error>(false);
	// Statically inserted tracks, keyed by name. A subscribe for one of these fans out
	// from the producer directly, skipping the on-demand request path.
	tracks = new Map<string, TrackProducer>();
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

	// Consume-side hook installed by the wire layer (Subscriber.consume) to resolve
	// a track's immutable TRACK_INFO over a Track stream. Undefined on a published
	// broadcast, where info comes from the producer's accept() instead.
	#infoResolver?: (name: string) => Promise<TrackInfo>;

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
	 * Get a lazy {@link TrackConsumer} handle for a track on this broadcast.
	 *
	 * Sends nothing over the network until you call {@link TrackConsumer.subscribe}
	 * or {@link TrackConsumer.info}. Mirrors the Rust `BroadcastConsumer::track`.
	 */
	track(name: string): TrackConsumer {
		return new TrackConsumer(this, name);
	}

	/**
	 * Insert a track that is served directly, without an on-demand {@link requested}
	 * round-trip.
	 *
	 * Each {@link subscribe} for this name fans out from the producer, so a publisher
	 * can push tracks proactively instead of waiting to be asked. The caller owns the
	 * {@link TrackProducer} and writes its groups; when that producer closes, the entry
	 * is removed automatically. Throws on a duplicate live name. Mirrors the Rust
	 * `BroadcastProducer::insert_track`.
	 *
	 * The producer must commit its {@link TrackInfo} via `accept()` (or use
	 * {@link createTrack}, which does it for you); otherwise a subscriber's
	 * `info()` never resolves and the wire layer stalls before serving.
	 */
	insertTrack(track: TrackProducer): void {
		if (this.state.closed.peek()) {
			throw new Error(`broadcast is closed: ${this.state.closed.peek()}`);
		}

		const existing = this.state.tracks.get(track.name);
		if (existing && !existing.state.closed.peek()) {
			throw new Error(`duplicate track: ${track.name}`);
		}

		this.state.tracks.set(track.name, track);

		// Evict the entry once the track closes, unless it has since been replaced.
		const dispose = track.state.closed.subscribe((closed) => {
			if (!closed) return;
			if (this.state.tracks.get(track.name) === track) {
				this.state.tracks.delete(track.name);
			}
			dispose();
		});
	}

	/**
	 * Create a track, insert it into the broadcast, and return its {@link TrackProducer}.
	 *
	 * Commits the immutable {@link TrackInfo} up front, so a subscriber resolves
	 * {@link TrackConsumer.info} without an on-demand round-trip. Mirrors the Rust
	 * `BroadcastProducer::create_track`.
	 */
	createTrack(name: string, info: Partial<TrackInfo> = {}): TrackProducer {
		const track = new TrackProducer(name).accept(info);
		this.insertTrack(track);
		return track;
	}

	/** Remove a statically inserted track by name. Mirrors the Rust `BroadcastProducer::remove_track`. */
	removeTrack(name: string): void {
		this.state.tracks.delete(name);
	}

	/**
	 * Open a live subscription to a track, returning the {@link TrackSubscriber} the
	 * groups stream into. Called by the consuming application (usually via
	 * {@link TrackConsumer.subscribe}) and by the publishing wire layer to ask the
	 * application for a track to serve.
	 */
	subscribe(name: string, priority: number): TrackSubscriber {
		if (this.state.closed.peek()) {
			throw new Error(`broadcast is closed: ${this.state.closed.peek()}`);
		}

		// Fast path: a statically inserted track fans out a fresh subscriber, no
		// request. A stale (closed) entry is evicted and falls through to on-demand.
		const existing = this.state.tracks.get(name);
		if (existing) {
			if (!existing.state.closed.peek()) return existing.subscribe();
			this.state.tracks.delete(name);
		}

		// The subscriber (caller, reads) and the request's producer (other side,
		// writes) share one state.
		const state = new TrackState();
		this.state.requested.mutate((requested) => {
			requested.push(new TrackRequest(name, state, priority));
			// Sort the tracks by priority in ascending order (we will pop)
			requested.sort((a, b) => a.priority - b.priority);
		});

		return new TrackSubscriber(name, state);
	}

	/**
	 * Install the consume-side TRACK_INFO resolver (used by {@link TrackConsumer.info}).
	 * Called once by the wire layer when this broadcast is consumed. Internal.
	 */
	onTrackInfo(resolver: (name: string) => Promise<TrackInfo>): void {
		this.#infoResolver = resolver;
	}

	/** Resolve a track's immutable info, used by {@link TrackConsumer.info}. Internal. */
	resolveTrackInfo(name: string): Promise<TrackInfo> {
		// Consume side: a TRACK stream (lite-05+) answers it.
		if (this.#infoResolver) {
			return this.#infoResolver(name);
		}

		// A statically inserted track already committed its info; serve it directly.
		const existing = this.state.tracks.get(name);
		if (existing && !existing.state.closed.peek()) {
			return existing.info();
		}

		// Publish side: ask the application by triggering a TrackRequest it answers
		// with accept(TrackInfo); only the immutable properties are needed, so close
		// the request once they're known rather than serving any groups.
		if (this.state.closed.peek()) {
			return Promise.reject(new Error(`broadcast is closed: ${this.state.closed.peek()}`));
		}

		const state = new TrackState();
		this.state.requested.mutate((requested) => {
			requested.push(new TrackRequest(name, state, 0));
			requested.sort((a, b) => a.priority - b.priority);
		});

		return (async () => {
			try {
				for (;;) {
					const info = state.info.peek();
					if (info) return info;

					const closed = state.closed.peek();
					if (closed instanceof Error) throw closed;
					if (closed) throw new Error(`track rejected: ${name}`);

					await Signal.race(state.info, state.closed);
				}
			} finally {
				state.closed.set(true);
			}
		})();
	}

	/**
	 * Closes the writer and all associated tracks.
	 *
	 * @param abort - If provided, throw this exception instead of returning undefined.
	 */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);
		for (const req of this.state.requested.peek()) {
			req.reject(abort);
		}
		this.state.requested.mutate((requested) => {
			requested.length = 0;
		});
	}
}
