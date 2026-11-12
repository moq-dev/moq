/**
 * The lite-07 route identity an origin originates its own claims under.
 *
 * @module
 */
import { Signal } from "@moq/signals";

/** The largest lite-07 varint, which no origin advertises as a Seqno or Revision. */
const MAX = 2n ** 64n - 1n;

/** A fresh Node ID: random, non-zero, and with the most significant bit clear, which marks a qualified one. */
function fresh(): bigint {
	const buf = new BigUint64Array(1);
	crypto.getRandomValues(buf);
	const id = buf[0] & ((1n << 63n) - 1n);
	return id === 0n ? 1n : id;
}

/** The origin node an origin's claims name, and the seqno only it advances. */
export interface Identity {
	readonly node: bigint;
	readonly seqno: bigint;
}

/**
 * One origin's own route: the Node ID every session publishing it declares, the seqno only it
 * advances, and the revision numbering the changes to its claims. A browser forwards nothing, so
 * this is the only route it originates. Mirrors the declared identity of `route::Node` in
 * rs/moq-net.
 *
 * @internal
 */
export class Own {
	/** The current identity; a rotation replaces the node, and a seqno advance the seqno. */
	readonly identity = new Signal<Identity>({ node: fresh(), seqno: 0n });

	/** How the origin names itself in a route's Hops: the Node ID it started with, which a rotation keeps. */
	readonly hop = this.identity.peek().node;

	#table: () => object | undefined;
	#seen?: object;
	#revision = 0n;

	/** `table` reads the origin's claims, which a new value marks as the next revision. */
	constructor(table: () => object | undefined) {
		this.#table = table;
	}

	/** The Node ID the origin declares. */
	get node(): bigint {
		return this.identity.peek().node;
	}

	/**
	 * The revision of the origin's claims as `table` reads now. Read in the same task as the claims
	 * it certifies, so the two always agree.
	 */
	revision(): bigint {
		const table = this.#table();
		if (table !== this.#seen) {
			if (this.#seen !== undefined) {
				// A change that would reach the last revision rotates the identity instead.
				if (this.#revision + 1n >= MAX) this.rotate();
				else this.#revision += 1n;
			}
			this.#seen = table;
		}
		return this.#revision;
	}

	/** A session that carried this origin's route closed, changing a path to it: advance the seqno. */
	bump(): void {
		const { node, seqno } = this.identity.peek();
		if (seqno + 1n >= MAX) this.rotate();
		else this.identity.set({ node, seqno: seqno + 1n });
	}

	/**
	 * A peer asked for a route to `node` at least as new as `seqno`. Only the origin answers, by
	 * raising its seqno; a request for the last one rotates the identity. A request for another
	 * node names nothing this origin reaches, and is dropped.
	 */
	request(node: bigint, seqno: bigint): void {
		const current = this.identity.peek();
		if (node !== current.node || seqno <= current.seqno) return;
		if (seqno >= MAX) this.rotate();
		else this.identity.set({ node, seqno });
	}

	/** Take a fresh identity, with a fresh revision space, ending the old one everywhere. */
	rotate(): void {
		this.identity.set({ node: fresh(), seqno: 0n });
		this.#revision = 0n;
		this.#seen = this.#table();
	}
}
