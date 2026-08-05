/**
 * The retry schedule shared by every loop that re-attempts a failed operation.
 *
 * Two halves, kept apart on purpose. {@link isRetryable} answers *whether* an attempt is worth
 * repeating; {@link Backoff} answers *when*. A loop with only the second half retries deterministic
 * failures forever, which is the bug this module exists to prevent, so classify first and back off
 * second.
 *
 * @module
 */

/** Delay before the first retry, in milliseconds. */
const DEFAULT_INITIAL = 1000;
/** Multiplier applied to the delay after each failure. */
const DEFAULT_MULTIPLIER = 2;
/** Ceiling on the delay, in milliseconds. */
const DEFAULT_MAX = 30000;
/** How long to keep retrying before giving up, in milliseconds. */
const DEFAULT_TIMEOUT = 300000;

/**
 * How long to wait between attempts, and how long to keep making them.
 *
 * The defaults suit a long-lived connection: a second before the first retry, doubling to a
 * half-minute ceiling, giving up after five minutes.
 */
export type BackoffProps = {
	/** Delay in milliseconds before the first retry (default: 1000). */
	initial?: DOMHighResTimeStamp;

	/** Multiplier applied to the delay after each failure (default: 2). */
	multiplier?: number;

	/** Ceiling on the delay in milliseconds, however many failures have piled up (default: 30000). */
	max?: DOMHighResTimeStamp;

	/**
	 * How long to keep retrying before giving up, in milliseconds (default: 300000, five minutes).
	 * Measured from the first delay after a {@link Backoff.reset}. Zero retries forever, which only
	 * belongs in a supervisor whose job is to outlive an outage.
	 */
	timeout?: DOMHighResTimeStamp;
};

/**
 * A failure that a retry cannot clear.
 *
 * Throw this instead of a plain `Error` when the next attempt is byte-for-byte the same as the one
 * that just failed: a relay speaking a protocol this build doesn't, a certificate that won't parse,
 * an option combination that leaves no usable transport. {@link isRetryable} reports `false` for it,
 * so a reconnect loop surfaces it rather than repeating it every few seconds.
 *
 * @public
 */
export class Terminal extends Error {
	constructor(message: string, options?: { cause?: unknown }) {
		super(message, options);
		this.name = "Terminal";
	}
}

/**
 * Whether repeating the failed operation could plausibly succeed with nothing else changing.
 *
 * Only {@link Terminal} is treated as settled. Unlike the Rust side, which classifies an error enum
 * variant by variant, the browser hands back whatever the platform threw: a `WebTransportError`, a
 * `DOMException`, an `AggregateError` wrapping a lost race, a bare `Error` from a relay. Defaulting
 * those to terminal would strand a connection that a retry would have recovered, so the burden sits
 * on whoever *knows* a failure is settled to say so.
 */
export function isRetryable(err: unknown): boolean {
	if (err instanceof Terminal) return false;

	// `Promise.any` rejects with every transport's failure at once; the attempt is worth repeating
	// if any of them was.
	if (err instanceof AggregateError) return err.errors.some(isRetryable);

	return true;
}

/**
 * A capped exponential backoff with jitter and a give-up budget.
 *
 * Each delay is drawn from the top half of the current window (equal jitter), so a fleet that fails
 * together doesn't retry together, while still waiting at least half the escalating delay. The
 * window grows by {@link BackoffProps.multiplier} per failure up to {@link BackoffProps.max}, and
 * {@link BackoffProps.timeout} bounds the whole sequence.
 *
 * Call {@link delay} after each failure and {@link reset} after a success worth trusting. Nothing
 * else may own a competing schedule for the same operation: an outer supervisor that rebuilds an
 * inner loop restarts its backoff at the initial delay and the escalation never happens.
 *
 * @public
 */
export class Backoff {
	readonly #initial: DOMHighResTimeStamp;
	readonly #multiplier: number;
	readonly #max: DOMHighResTimeStamp;
	readonly #timeout: DOMHighResTimeStamp;

	/** The current window's upper bound, grown per failure. */
	#window: DOMHighResTimeStamp;

	/** When the budget runs out, or undefined while the sequence hasn't started. */
	#deadline: DOMHighResTimeStamp | undefined;

	constructor(props?: BackoffProps) {
		this.#initial = props?.initial ?? DEFAULT_INITIAL;
		this.#multiplier = props?.multiplier ?? DEFAULT_MULTIPLIER;
		this.#max = props?.max ?? DEFAULT_MAX;
		this.#timeout = props?.timeout ?? DEFAULT_TIMEOUT;
		this.#window = this.#initial;
	}

	/** How long to wait before the next attempt, or undefined once the budget is spent. */
	delay(): DOMHighResTimeStamp | undefined {
		if (this.#timeout > 0) {
			const now = performance.now();
			if (this.#deadline === undefined) {
				// The first delay of a sequence starts the clock, so a loop that ran healthy for
				// hours still gets its full budget when it finally does fail.
				this.#deadline = now + this.#timeout;
			} else if (now >= this.#deadline) {
				return undefined;
			}
		}

		// Equal jitter: at least half the window, never more than all of it.
		const delay = this.#window / 2 + Math.random() * (this.#window / 2);
		this.#window = Math.min(this.#window * this.#multiplier, this.#max);

		return delay;
	}

	/**
	 * Start over: the next delay is {@link BackoffProps.initial} again and the budget is full.
	 *
	 * Only call this after an outcome that says the earlier failures no longer describe reality: a
	 * session that stayed up, a request that succeeded, a changed destination. Resetting on an
	 * attempt that failed immediately turns the escalation into a tight loop.
	 */
	reset(): void {
		this.#window = this.#initial;
		this.#deadline = undefined;
	}
}
