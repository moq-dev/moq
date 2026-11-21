import { now, sleepUntil, timeout } from "./timer.ts";
import { Duration } from "./types.ts";

/** What `until` reads, when it stops, and how long it may take. */
export interface Wait<T> {
	/** Observe the state once, bounding any request inside by `left`, the watchdog's remaining budget. */
	read: (left: Duration) => Promise<T>;
	/** Whether the observed state is the one being waited on. */
	ready: (state: T) => boolean;
	/** End a wait that never settles; never the expected path. */
	watchdog: Duration;
	/** Pause between reads, which must be positive; one second by default. */
	every?: Duration;
}

/** A wait the watchdog ended, carrying the last state read, or `undefined` if no read finished. */
export class Unsettled<T> extends Error {
	/** The last state read before the watchdog passed. */
	readonly last: T | undefined;

	/** Build the failure for a wait that never settled within `watchdog`. */
	constructor(watchdog: Duration, last: T | undefined) {
		super(`never settled within ${watchdog}; last read: ${describe(last)}`);
		this.name = "Unsettled";
		this.last = last;
	}
}

/**
 * Read until `ready` accepts the state and return it, or an `Unsettled` once the watchdog passes.
 *
 * The watchdog races each read, so it holds even for a read that ignores `left`.
 * A read that throws before the deadline ends the wait with that error; one that
 * throws once the deadline has passed, such as on its own `left` budget, is the watchdog.
 */
export async function until<T>(wait: Wait<T>): Promise<T | Unsettled<T>> {
	const every = wait.every ?? Duration.secs(1);
	// A zero pause would read in a loop that never yields, starving what it waits on.
	if (every.isZero()) throw new RangeError("until period must be positive");
	const deadline = now().saturatingAdd(wait.watchdog);
	let last: T | undefined;
	for (;;) {
		const left = deadline.since(now());
		if (left.isZero()) return new Unsettled(wait.watchdog, last);
		try {
			const pending = wait.read(left);
			// Bounded by what is left after the read's synchronous work, so the deadline stays put.
			last = await timeout(deadline.since(now()), pending);
		} catch (e) {
			// Decided by the clock, not the error's type, so any timeout the read uses counts.
			if (now().ge(deadline)) return new Unsettled(wait.watchdog, last);
			throw e;
		}
		if (wait.ready(last)) return last;
		await sleepUntil(now().saturatingAdd(every).min(deadline));
	}
}

// A state in a failure message, cut short so a whole response body cannot flood it.
function describe(state: unknown): string {
	let text: string;
	try {
		text = typeof state === "string" ? state : (JSON.stringify(state) ?? String(state));
	} catch {
		text = String(state);
	}
	return text.length > 500 ? `${text.slice(0, 500)}...` : text;
}
