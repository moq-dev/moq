import { race } from "@moq/signals";

/**
 * The one entry point every stream a session opens goes through.
 *
 * A peer grants a bounded number of concurrent streams in each direction, and an open beyond
 * that waits for the peer to grant more: a browser keeps `createUnidirectionalStream()`
 * pending, as `@moq/qmux` does. Whichever create the transport admits first is handed to the
 * most important open still waiting, ranked by the send order its stream will have, so
 * returning credit goes where it matters rather than to whatever asked first.
 *
 * Every open starts a create of its own, so opens never queue behind each other while credit
 * is free. An open that gives up leaves its create running: the stream goes to the next open
 * in line, or is kept for the next open to arrive.
 *
 * Chrome ignores a `sendOrder` set after a create, so there a stream handed to another open
 * keeps the send order it was created at.
 */

const live = Symbol("live");

/** A send order, or a way to read it while it can still move. */
export type Rank = number | (() => number);

interface Waiting<T> {
	rank: () => number;
	resolve: (stream: T) => void;
	reject: (err: unknown) => void;
}

/** An admitted stream, with the send order it was created at. */
interface Admitted<T> {
	stream: T;
	order: number;
}

/** The opens of one direction waiting for the peer's stream credit. */
class Line<T> {
	readonly #create: (sendOrder: number) => Promise<T>;
	readonly #rank: (stream: T, sendOrder: number) => void;
	/** In arrival order, which breaks a tie in rank. */
	#waiting: Waiting<T>[] = [];
	/** Streams admitted after the open that created them gave up. */
	#spare: Admitted<T>[] = [];
	/** Creates still waiting on the transport, never more than the opens waiting. */
	#creating = 0;

	constructor(create: (sendOrder: number) => Promise<T>, rank: (stream: T, sendOrder: number) => void) {
		this.#create = create;
		this.#rank = rank;
	}

	/**
	 * Wait for a stream at `rank`, read again whenever a stream is handed out, or undefined once
	 * `cancel` settles first.
	 */
	open(rank: Rank): Promise<T>;
	open(rank: Rank, cancel: Promise<unknown>): Promise<T | undefined>;
	async open(rank: Rank, cancel?: Promise<unknown>): Promise<T | undefined> {
		const current = typeof rank === "number" ? () => rank : rank;
		// A caller already cancelled takes nothing, not even a stream that is free now. A fresh
		// settled promise, observed after `cancel`, loses only to a cancel already settled.
		if (cancel && (await race([cancel, Promise.resolve(live)])) !== live) return undefined;
		if (this.#waiting.length === 0) {
			const spare = this.#spare.pop();
			if (spare) return this.#handed(spare, current());
		}

		let waiting!: Waiting<T>;
		const ticket = new Promise<T>((resolve, reject) => {
			waiting = { rank: current, resolve, reject };
		});
		this.#waiting.push(waiting);

		// An open that gave up leaves its create running for the next one, so cancellation
		// churn never queues more creates than opens wait.
		if (this.#creating < this.#waiting.length) {
			const order = current();
			this.#creating++;
			this.#create(order).then(
				(stream) => {
					this.#creating--;
					this.#admit({ stream, order });
				},
				(err: unknown) => {
					this.#creating--;
					this.#refuse(err);
				},
			);
		}

		if (!cancel) return ticket;
		// `race`, so a cancel shared across many opens gains no reaction per open.
		let opened: T | undefined;
		try {
			opened = (await race([ticket, cancel])) as T | undefined;
		} catch (err: unknown) {
			if (this.#leave(waiting)) throw err;
			return ticket;
		}
		if (opened !== undefined || !this.#leave(waiting)) return opened ?? ticket;
		return undefined;
	}

	/** Leave the line, or false once a stream was already handed over. */
	#leave(waiting: Waiting<T>): boolean {
		const index = this.#waiting.indexOf(waiting);
		if (index === -1) return false;
		this.#waiting.splice(index, 1);
		return true;
	}

	/** Hand an admitted stream to the highest ranked open in line, or keep it for the next. */
	#admit(admitted: Admitted<T>) {
		let best = -1;
		let order = 0;
		for (let i = 0; i < this.#waiting.length; i++) {
			const rank = this.#waiting[i].rank();
			if (best === -1 || rank > order) {
				best = i;
				order = rank;
			}
		}
		if (best === -1) {
			this.#spare.push(admitted);
			return;
		}
		const [next] = this.#waiting.splice(best, 1);
		next.resolve(this.#handed(admitted, order));
	}

	/** A failed create fails the newest open in line: one more open than streams it can have. */
	#refuse(err: unknown) {
		this.#waiting.pop()?.reject(err);
	}

	#handed(admitted: Admitted<T>, sendOrder: number): T {
		if (admitted.order !== sendOrder) this.#rank(admitted.stream, sendOrder);
		return admitted.stream;
	}
}

type SendStream = WritableStream<Uint8Array> & { sendOrder?: number };

/** The lines of one session's opens. */
interface Lines {
	uni: Line<SendStream>;
	bi: Line<WebTransportBidirectionalStream>;
}

const sessions = new WeakMap<WebTransport, Lines>();

/** The lines `quic`'s opens wait in, shared by everything opening on it. */
export function lines(quic: WebTransport): Lines {
	let lines = sessions.get(quic);
	if (lines) return lines;
	// `@types/web` doesn't declare `waitUntilAvailable` yet. Waiting is what keeps an open in
	// line rather than refused. 0 is the transport's default order, so it is left unset.
	const options = (sendOrder: number) =>
		sendOrder === 0 ? { waitUntilAvailable: true } : { sendOrder, waitUntilAvailable: true };
	lines = {
		uni: new Line(
			(sendOrder) => quic.createUnidirectionalStream(options(sendOrder)) as Promise<SendStream>,
			(stream, sendOrder) => {
				stream.sendOrder = sendOrder;
			},
		),
		bi: new Line(
			(sendOrder) => quic.createBidirectionalStream(options(sendOrder)),
			(stream, sendOrder) => {
				(stream.writable as SendStream).sendOrder = sendOrder;
			},
		),
	};
	sessions.set(quic, lines);
	return lines;
}
