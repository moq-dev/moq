import { Duration } from "@moq/clock";

/** How many reads the minimum covers. */
const WINDOW = 30;
/** The smallest change reported. */
const FLOOR = Duration.millis(5);

/**
 * A link's min RTT, as the route layer adds it to the routes learned over the link.
 * Mirrors `route::LinkRtt` in rs/moq-net.
 *
 * Reads a link's smoothed RTT once per {@link LinkRtt.INTERVAL} and reports the lowest of
 * the last 30 reads in whole milliseconds, changed only once it moves past the larger of
 * 5 ms and a quarter of the value reported.
 *
 * The smoothed RTT follows every queue, and a lifetime minimum never rises after a path
 * change; a windowed minimum ignores a queue shorter than the window and turns a path
 * change into one new value.
 *
 * @internal
 */
export class LinkRtt {
	/** How often to {@link read} the smoothed RTT. */
	static readonly INTERVAL = Duration.secs(1);

	#reads: Duration[] = [];
	#reported: Duration | undefined;
	/** Reads in a row that found no RTT. */
	#misses = 0;

	/** The min RTT last reported. */
	get reported(): Duration | undefined {
		return this.#reported;
	}

	/**
	 * Whether to keep reading: false once a window of reads found no RTT, since a transport
	 * that reports none that long, past a first ping, never will.
	 */
	get reading(): boolean {
		return this.#misses < WINDOW;
	}

	/**
	 * Take one read of the smoothed RTT, `undefined` when the transport reports none, and
	 * return the min RTT to report.
	 */
	read(rtt: Duration | undefined): Duration | undefined {
		if (rtt === undefined) {
			this.#reads = [];
			this.#reported = undefined;
			this.#misses += 1;
			return undefined;
		}
		this.#misses = 0;
		if (this.#reads.length === WINDOW) this.#reads.shift();
		this.#reads.push(rtt);
		const lowest = this.#reads.reduce((a, b) => a.min(b));
		const min = Duration.millis(Math.ceil(Math.floor(lowest.asMicros()) / 1000));
		const reported = this.#reported;
		if (!reported || min.absDiff(reported).gt(reported.div(4).max(FLOOR))) this.#reported = min;
		return this.#reported;
	}
}
