import { Session, SessionCode } from "../error.ts";

/**
 * How many live requests a peer may hold on one session, as moq-net's `session::Limits`.
 * Subscriptions, FETCHes, and listing PUBLISHes have no cap: each holds a bidi stream, so stream
 * credit bounds them.
 * @internal
 */
export interface Limits {
	/** Broadcasts (moq-lite) or namespaces (moq-transport) the peer may have announced to us. */
	announces: number;
}

/** The default limits, matching moq-net's. @internal */
export const LIMITS: Readonly<Limits> = Object.freeze({ announces: 100_000 });

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
		if (this.tryAcquire()) return;
		const err = new Session(SessionCode.TooManyRequests, { reason: this.#reason });
		this.#quic?.close({ closeCode: err.code, reason: this.#reason });
		throw err;
	}

	/** Take one slot, or leave the session alone and return false at the cap. */
	tryAcquire(): boolean {
		if (this.#live >= this.#max) return false;
		this.#live += 1;
		return true;
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
