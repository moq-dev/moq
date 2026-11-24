import { Signal } from "@moq/signals";

/** One dial of a connection, waiting for the tokens it presents. */
export class Dial {
	/** Where the dial goes: the connection's URL, or the relay a GOAWAY redirected it to. */
	readonly url: URL;

	/** Aborted once the connection no longer waits for this dial's tokens, such as on close. */
	readonly signal: AbortSignal;

	readonly #tokens = Promise.withResolvers<readonly string[]>();

	/** @internal */
	constructor(url: URL, signal: AbortSignal) {
		this.url = url;
		this.signal = signal;
		// A dial its connection abandoned rejects unread.
		this.#tokens.promise.catch(() => {});
		if (signal.aborted) this.#tokens.reject(signal.reason);
		else signal.addEventListener("abort", () => this.#tokens.reject(signal.reason), { once: true });
	}

	/** Present `tokens` on this dial, as a fixed list would be; the first answer counts. */
	present(tokens: readonly string[]): void {
		this.#tokens.resolve([...tokens]);
	}

	/** Fail this dial with `err`, as an unreachable peer would: the connection retries or gives up as for any failed dial. */
	fail(err: Error): void {
		this.#tokens.reject(err);
	}

	/** @internal */
	get tokens(): Promise<readonly string[]> {
		return this.#tokens.promise;
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
		const dial = new Dial(url, signal);
		this.#waiting.push(dial);
		this.#queued.update((n) => n + 1);
		return dial.tokens;
	}

	/** End {@link next}, failing every dial still waiting. @internal */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const dial of this.#waiting.splice(0)) dial.fail(new Error("connection closed"));
		this.#queued.update((n) => n + 1);
	}
}
