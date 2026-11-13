import { Duration, sleep } from "@moq/clock";
import { type Getter, race } from "@moq/signals";
import type { Probe } from "./connection/stats.ts";
import { transportStats } from "./connection/stats.ts";
import { transportOf } from "./connection/transport.ts";

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
 *
 * The session also lists the group streams it has open, each with its send order and whether
 * the peer has acknowledged everything written to it. A browser reports no stream credit, so a
 * unidirectional open that has waited about one round trip resets the lowest one below it to
 * make room, at most one reset in flight per waiting open, since each frees credit only once
 * the peer's MAX_STREAMS arrives.
 */

const live = Symbol("live");

/** A send order, or a way to read it while it can still move. */
export type Rank = number | (() => number);

interface Waiting<T> {
	rank: () => number;
	resolve: (stream: T) => void;
	reject: (err: unknown) => void;
	/** Aborted once the open leaves the line, ending its wait to make room. */
	left: AbortController;
	/** Whether it has waited long enough to make room. */
	impatient: boolean;
}

/** What a line resets to make room with, once an open has waited long enough. */
interface Room {
	streams: OpenStreams;
	/** About one round trip: how long an open waits for credit before making room. */
	patience: () => Promise<Duration>;
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
	readonly #room?: Room;
	/**
	 * Streams reset to make room whose credit has yet to reach an open, never more than the
	 * opens waiting: the first this many in line are owed one.
	 */
	#resets = 0;

	constructor(create: (sendOrder: number) => Promise<T>, rank: (stream: T, sendOrder: number) => void, room?: Room) {
		this.#create = create;
		this.#rank = rank;
		this.#room = room;
	}

	/**
	 * Wait for a stream at `rank`, read again whenever a stream is handed out, or undefined once
	 * `cancel` settles or aborts first.
	 */
	open(rank: Rank): Promise<T>;
	open(rank: Rank, cancel: Promise<unknown> | AbortSignal): Promise<T | undefined>;
	async open(rank: Rank, cancel?: Promise<unknown> | AbortSignal): Promise<T | undefined> {
		const current = typeof rank === "number" ? () => rank : rank;
		// A caller already cancelled takes nothing, not even a stream that is free now. A signal
		// says so at once, so the open keeps its place in call order. A fresh settled promise,
		// observed after `cancel`, loses only to a cancel already settled.
		if (cancel instanceof AbortSignal) {
			if (cancel.aborted) return undefined;
		} else if (cancel && (await race([cancel, Promise.resolve(live)])) !== live) return undefined;
		if (this.#waiting.length === 0) {
			const spare = this.#spare.pop();
			if (spare) return this.#handed(spare, current());
		}

		let waiting!: Waiting<T>;
		const ticket = new Promise<T>((resolve, reject) => {
			waiting = { rank: current, resolve, reject, left: new AbortController(), impatient: false };
		});
		this.#waiting.push(waiting);
		if (this.#room) {
			// An open ranked above an impatient one takes its place among those a reset is owed to.
			this.#makeRoom(this.#room);
			void this.#impatient(this.#room, waiting);
		}

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
		const signalled = cancel instanceof AbortSignal ? aborted(cancel) : undefined;
		// `race`, so a cancel shared across many opens gains no reaction per open.
		let opened: T | undefined;
		try {
			opened = (await race([ticket, signalled?.promise ?? cancel])) as T | undefined;
		} catch (err: unknown) {
			if (this.#leave(waiting)) throw err;
			return ticket;
		} finally {
			signalled?.done();
		}
		if (opened !== undefined || !this.#leave(waiting)) return opened ?? ticket;
		return undefined;
	}

	/** Leave the line, or false once a stream was already handed over. */
	#leave(waiting: Waiting<T>): boolean {
		const index = this.#waiting.indexOf(waiting);
		if (index === -1) return false;
		this.#remove(index);
		return true;
	}

	#remove(index: number): Waiting<T> {
		const [left] = this.#waiting.splice(index, 1);
		left.left.abort();
		// Never more resets owed than opens left to owe them to.
		this.#resets = Math.min(this.#resets, this.#waiting.length);
		return left;
	}

	/** Make room once `waiting` has waited about a round trip without a stream. */
	async #impatient(room: Room, waiting: Waiting<T>) {
		try {
			const patience = await room.patience();
			if (waiting.left.signal.aborted) return;
			await sleep(patience, { signal: waiting.left.signal, ref: false });
		} catch {
			// Left the line first.
			return;
		}
		waiting.impatient = true;
		this.#makeRoom(room);
	}

	/**
	 * Reset the lowest open group stream below each waiting open not yet owed a reset that has
	 * waited long enough.
	 */
	#makeRoom(room: Room) {
		// The highest rank first, and the earliest arrival among equals.
		const owed = this.#waiting
			.map((waiting) => ({ rank: waiting.rank(), impatient: waiting.impatient }))
			.sort((a, b) => b.rank - a.rank)
			.slice(this.#resets)
			.filter((waiting) => waiting.impatient)
			.map((waiting) => waiting.rank);
		const open = room.streams.list();
		for (const rank of owed) {
			const lowest = open.shift();
			// The opens behind rank lower still.
			if (!lowest || lowest.sendOrder >= rank) break;
			console.debug(`reset a lower stream to make room: sendOrder=${lowest.sendOrder} waiting=${rank}`);
			lowest.preempt();
			this.#resets++;
		}
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
		// Whichever open the credit reaches, a reset's credit may be the one it took.
		this.#resets = Math.max(this.#resets - 1, 0);
		const next = this.#remove(best);
		// A rank read again may have moved an impatient open out of those still owed a reset.
		if (this.#room) this.#makeRoom(this.#room);
		next.resolve(this.#handed(admitted, order));
	}

	/** A failed create fails the newest open in line: one more open than streams it can have. */
	#refuse(err: unknown) {
		if (this.#waiting.length > 0) this.#remove(this.#waiting.length - 1).reject(err);
	}

	#handed(admitted: Admitted<T>, sendOrder: number): T {
		if (admitted.order !== sendOrder) this.#rank(admitted.stream, sendOrder);
		return admitted.stream;
	}
}

/** Resolve once `signal` aborts, until `done` stops listening, so a long-lived signal gathers no listeners. */
function aborted(signal: AbortSignal): { promise: Promise<void>; done: () => void } {
	const { promise, resolve } = Promise.withResolvers<void>();
	const onAbort = () => resolve();
	signal.addEventListener("abort", onAbort, { once: true });
	return { promise, done: () => signal.removeEventListener("abort", onAbort) };
}

type SendStream = WritableStream<Uint8Array> & { sendOrder?: number };

/** One of a session's open group streams. */
export interface OpenStream {
	/** The send order the stream has now, where HIGHER values are sent first. */
	readonly sendOrder: number;
	/**
	 * Whether the peer has acknowledged every byte written to it so far, or undefined when the
	 * transport cannot tell.
	 */
	acknowledged(): Promise<boolean | undefined>;
	/**
	 * Reset the stream to make room for a higher one, failing its writer as too far behind, or on
	 * lite-07 as yielded, pausing its group for a later stream to continue.
	 */
	preempt(): void;
}

/** The group streams a session has open, from when each opens until it is closed or reset. */
export class OpenStreams {
	/**
	 * Whether the transport reports acknowledgements, known when the session starts:
	 * `@moq/qmux` does, a browser's WebTransport cannot.
	 */
	readonly acknowledges: boolean;
	readonly #open = new Set<OpenStream>();

	constructor(acknowledges: boolean) {
		this.acknowledges = acknowledges;
	}

	add(stream: OpenStream) {
		this.#open.add(stream);
	}

	delete(stream: OpenStream) {
		this.#open.delete(stream);
	}

	/** The open group streams, the lowest send order first, and the earliest opened among equals. */
	list(): OpenStream[] {
		return [...this.#open].sort((a, b) => a.sendOrder - b.sendOrder);
	}
}

/** The lines of one session's opens, and the group streams it has open. */
interface Lines {
	uni: Line<SendStream>;
	bi: Line<WebTransportBidirectionalStream>;
	streams: OpenStreams;
	/** The peer's PROBE estimates, whose RTT stands in for a transport that reports none. */
	probe?: Getter<Probe>;
}

const sessions = new WeakMap<WebTransport, Lines>();

/** The lines `quic`'s opens wait in, and its open group streams, shared by everything opening on it. */
export function lines(quic: WebTransport): Lines {
	const lines = sessions.get(quic);
	if (lines) return lines;
	// `@types/web` doesn't declare `waitUntilAvailable` yet. Waiting is what keeps an open in
	// line rather than refused. 0 is the transport's default order, so it is left unset.
	const options = (sendOrder: number) =>
		sendOrder === 0 ? { waitUntilAvailable: true } : { sendOrder, waitUntilAvailable: true };
	const streams = new OpenStreams(transportOf(quic) === "websocket");
	// Before any RTT sample, wait 100 ms. A transport may report 0 until its first sample.
	// A zero RTT is a transport that has not sampled yet.
	const sampled = (rtt?: Duration) => (rtt?.isZero() ? undefined : rtt);
	const patience = async () =>
		sampled((await transportStats(quic)).rtt) ?? sampled(session.probe?.peek().rtt) ?? Duration.millis(100);
	const session: Lines = {
		uni: new Line(
			(sendOrder) => quic.createUnidirectionalStream(options(sendOrder)) as Promise<SendStream>,
			(stream, sendOrder) => {
				stream.sendOrder = sendOrder;
			},
			{ streams, patience },
		),
		bi: new Line(
			(sendOrder) => quic.createBidirectionalStream(options(sendOrder)),
			(stream, sendOrder) => {
				(stream.writable as SendStream).sendOrder = sendOrder;
			},
		),
		streams,
	};
	sessions.set(quic, session);
	return session;
}
