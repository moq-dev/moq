import { Signal } from "@moq/signals";

/** How the app answered a dial: the tokens to present, or why the connection should stop. @internal */
export type Answer = { tokens: readonly string[] } | { refused: Error };

// Set by Dial's static block, which alone may call its private constructor.
let makeDial: (url: URL, signal: AbortSignal, failure?: Error) => [Dial, Promise<Answer>];

/** One dial of a connection, waiting for the tokens it presents. */
export class Dial {
	/** Where the dial goes: the connection's URL, or the relay a GOAWAY redirected it to. */
	readonly url: URL;

	/**
	 * Why the previous attempt or session ended, such as a refused credential or a redirect before
	 * the session was healthy, or undefined when nothing failed since a session last served, as
	 * for the first dial or a healthy session's GOAWAY handover.
	 */
	readonly failure?: Error;

	/** Aborted once the connection no longer waits for this dial's tokens: on close, or past the connect timeout. */
	readonly signal: AbortSignal;

	readonly #answer = Promise.withResolvers<Answer>();

	private constructor(url: URL, signal: AbortSignal, failure?: Error) {
		this.url = url;
		this.signal = signal;
		this.failure = failure;
		// A dial its connection abandoned rejects unread.
		this.#answer.promise.catch(() => {});
		if (signal.aborted) this.#answer.reject(signal.reason);
		else signal.addEventListener("abort", () => this.#answer.reject(signal.reason), { once: true });
	}

	static {
		makeDial = (url, signal, failure) => {
			const dial = new Dial(url, signal, failure);
			return [dial, dial.#answer.promise];
		};
	}

	/**
	 * Present `tokens` on this dial, as a fixed list would be; the first answer counts. Only the
	 * first, the connection credential, is this dial's alone: the rest join the connection's
	 * shared set, which a successor attaches to before its own answer replaces them, so a
	 * single-use token belongs first.
	 */
	present(tokens: readonly string[]): void {
		this.#answer.resolve({ tokens: [...tokens] });
	}

	/**
	 * Stop the connection with `err` instead of dialing, as for a {@link failure} no retry can
	 * repair. A probe back to WebTransport, which the serving session does without, is skipped instead.
	 */
	fail(err: Error): void {
		this.#answer.resolve({ refused: err });
	}
}

/**
 * Tokens a connection asks for at each dial, for a credential that must be fresh when it is
 * presented, such as a proof a peer admits once and only shortly after it was signed. Pass it as
 * a connection's `tokens` and answer each {@link next} dial: every dial waits for its answer,
 * whatever starts it, a reconnect, a GOAWAY's handover, a page resuming, or a probe back to
 * WebTransport, so none presents tokens signed for another.
 *
 * Answering is also how a connection retries: unlike fixed tokens, which stop at the first
 * failure of a URL never reached, it backs off and asks again, with the reason on
 * {@link Dial.failure}, until a dial {@link Dial.fail}s.
 */
export class Dials {
	readonly #waiting: Dial[] = [];
	readonly #queued = new Signal(0);
	#claimed = false;
	#closed = false;

	/** Whether a connection took these dials, which serve one. @internal */
	get claimed(): boolean {
		return this.#claimed;
	}

	/** Take these dials for one connection. @internal */
	claim(): void {
		this.#claimed = true;
	}

	/** The next dial waiting for its tokens, or undefined once the connection closed. */
	async next(): Promise<Dial | undefined> {
		for (;;) {
			for (let dial = this.#waiting.shift(); dial; dial = this.#waiting.shift()) {
				if (!dial.signal.aborted) return dial;
			}
			if (this.#closed) return undefined;
			await this.#queued.changed();
		}
	}

	/** Ask for the tokens of a dial to `url`, until `signal` aborts, which rejects it. @internal */
	request(url: URL, signal: AbortSignal, failure?: Error): Promise<Answer> {
		if (this.#closed) return Promise.reject(new Error("connection closed"));
		const [dial, answer] = makeDial(url, signal, failure);
		// Dials the loop abandoned unread go now, so an app that stops reading holds at most one.
		const live = this.#waiting.filter((waiting) => !waiting.signal.aborted);
		this.#waiting.splice(0, this.#waiting.length, ...live, dial);
		this.#queued.update((n) => n + 1);
		return answer;
	}

	/** End {@link next}, failing every dial still waiting. @internal */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const dial of this.#waiting.splice(0)) dial.fail(new Error("connection closed"));
		this.#queued.update((n) => n + 1);
	}
}
