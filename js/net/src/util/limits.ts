import { Session, SessionCode } from "../error.ts";

/** How many live requests a peer may hold on one session, as moq-net's `session::Limits`. @internal */
export interface Limits {
	/** Broadcasts (moq-lite) or namespaces (moq-transport) the peer may have announced to us. */
	announces: number;
	/** Subscriptions the peer may hold on our broadcasts, a TRACK stream sharing one with its SUBSCRIBE. */
	subscriptions: number;
}

/** moq-net's default limits. @internal */
export const LIMITS: Readonly<Limits> = Object.freeze({ announces: 100_000, subscriptions: 10_000 });

/** A session's count of live peer requests, which closes the session past its cap. @internal */
export class Slots {
	#live = 0;
	#max: number;
	#quic?: WebTransport;
	#reason: string;

	constructor(max: number, { quic, reason }: { quic?: WebTransport; reason: string }) {
		this.#max = max;
		this.#quic = quic;
		this.#reason = reason;
	}

	/** Take one slot, or close the session with TOO_MANY_REQUESTS and throw at the cap. */
	acquire() {
		if (this.#live >= this.#max) {
			const err = new Session(SessionCode.TooManyRequests, { reason: this.#reason });
			this.#quic?.close({ closeCode: err.code, reason: this.#reason });
			throw err;
		}
		this.#live += 1;
	}

	/** Give back `count` slots. */
	release(count = 1) {
		this.#live -= count;
	}
}

/** A map whose every entry holds one slot, as moq-net charges each live advertisement. @internal */
export class SlotMap<K, V> extends Map<K, V> {
	#slots: Slots;

	constructor(slots: Slots) {
		super();
		this.#slots = slots;
	}

	override set(key: K, value: V): this {
		if (!this.has(key)) this.#slots.acquire();
		return super.set(key, value);
	}

	override delete(key: K): boolean {
		if (!super.delete(key)) return false;
		this.#slots.release();
		return true;
	}

	override clear() {
		this.#slots.release(this.size);
		super.clear();
	}
}

type Kind = "track" | "subscribe";
type Counts = { track: number; subscribe: number };

/**
 * A session's subscription cap, charged per track as moq-net does.
 *
 * A TRACK stream held open and the SUBSCRIBE that follows it are one interest in the track,
 * so a track costs the larger of its two counts: a peer at the cap can still subscribe to
 * the tracks it holds.
 *
 * @internal
 */
export class Interests {
	#slots: Slots;
	#tracks = new Map<string, Map<string, Counts>>();

	constructor(slots: Slots) {
		this.#slots = slots;
	}

	/** Charge one request for `broadcast`'s `track` until the returned handle is released. */
	acquire(broadcast: string, track: string, kind: Kind): Interest {
		let tracks = this.#tracks.get(broadcast);
		const counts = tracks?.get(track) ?? { track: 0, subscribe: 0 };
		if (counts[kind] >= counts[kind === "track" ? "subscribe" : "track"]) this.#slots.acquire();
		counts[kind] += 1;
		if (!tracks) {
			tracks = new Map();
			this.#tracks.set(broadcast, tracks);
		}
		tracks.set(track, counts);
		return new Interest(() => this.#release(broadcast, track, kind));
	}

	#release(broadcast: string, track: string, kind: Kind) {
		const tracks = this.#tracks.get(broadcast);
		const counts = tracks?.get(track);
		if (!tracks || !counts) throw new Error("interest already released");
		if (counts[kind] > counts[kind === "track" ? "subscribe" : "track"]) this.#slots.release();
		counts[kind] -= 1;
		if (counts.track === 0 && counts.subscribe === 0) {
			tracks.delete(track);
			if (tracks.size === 0) this.#tracks.delete(broadcast);
		}
	}
}

/** One charged request, released at most once. @internal */
export class Interest {
	#release?: () => void;

	constructor(release: () => void) {
		this.#release = release;
	}

	release() {
		this.#release?.();
		this.#release = undefined;
	}
}
