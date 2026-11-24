import { Signal } from "@moq/signals";

// Set by Dial's static block, which alone may call its private constructor.
let makeDial: (url: URL, signal: AbortSignal) => [Dial, Promise<readonly string[]>];

/** One dial of a connection, waiting for the tokens it presents. */
export class Dial {
	/** Where the dial goes: the connection's URL, or the relay a GOAWAY redirected it to. */
	readonly url: URL;

	/** Aborted once the connection no longer waits for this dial's tokens, such as on close. */
	readonly signal: AbortSignal;

	readonly #tokens = Promise.withResolvers<readonly string[]>();

	private constructor(url: URL, signal: AbortSignal) {
		this.url = url;
		this.signal = signal;
		// A dial its connection abandoned rejects unread.
		this.#tokens.promise.catch(() => {});
		if (signal.aborted) this.#tokens.reject(signal.reason);
		else signal.addEventListener("abort", () => this.#tokens.reject(signal.reason), { once: true });
	}

	static {
		makeDial = (url, signal) => {
			const dial = new Dial(url, signal);
			return [dial, dial.#tokens.promise];
		};
	}

	/** Present `tokens` on this dial, as a fixed list would be; the first answer counts. */
	present(tokens: readonly string[]): void {
		this.#tokens.resolve([...tokens]);
	}

	/** Fail this dial with `err`, as an unreachable peer would: the connection retries or gives up as for any failed dial. */
	fail(err: Error): void {
		this.#tokens.reject(err);
	}
}

/**
 * Tokens a connection asks for at each dial, for a credential that must be fresh when it is
 * presented, such as a proof a peer admits once and only shortly after it was signed. Pass it as
 * a connection's `tokens` and answer each {@link next} dial: every dial waits for its answer,
 * whatever starts it, a reconnect, a GOAWAY's handover, a page resuming, or a probe back to
 * WebTransport, so none presents tokens signed for another.
 */
export class Dials {
	readonly #waiting: Dial[] = [];
	readonly #queued = new Signal(0);
	#closed = false;

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

	/** Ask for the tokens of a dial to `url`, until `signal` aborts. @internal */
	request(url: URL, signal: AbortSignal): Promise<readonly string[]> {
		if (this.#closed) return Promise.reject(new Error("connection closed"));
		const [dial, tokens] = makeDial(url, signal);
		this.#waiting.push(dial);
		this.#queued.update((n) => n + 1);
		return tokens;
	}

	/** End {@link next}, failing every dial still waiting. @internal */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const dial of this.#waiting.splice(0)) dial.fail(new Error("connection closed"));
		this.#queued.update((n) => n + 1);
	}
}
