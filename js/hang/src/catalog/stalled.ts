import { Time } from "@moq/net";

/** How many frame intervals of lag mark a rendition stalled. */
export const SET_INTERVALS = 3;

/** Consecutive on-time frames required to clear a stall. */
export const CLEAR_FRAMES = 3;

/** Frame interval used when the catalog does not advertise a framerate. */
export const DEFAULT_INTERVAL = Time.Micro.fromMilli(33 as Time.Milli);

/** One observation of a rendition's source versus what the transport has accepted. */
export interface Sample {
	/** This observation completes a newly accepted frame. */
	frame: boolean;
	/** Newest captured timestamp minus newest timestamp handed to the transport. */
	mediaLag: Time.Micro;
	/** Wall time since the source last delivered a frame, in microseconds. */
	quiet: Time.Micro;
	/** One frame interval. The set threshold is {@link SET_INTERVALS} of these. */
	interval: Time.Micro;
	/** Someone is subscribed to this rendition. */
	demand: boolean;
	/** Camera released / no live source. Never stalled. */
	idle: boolean;
}

/** A positive frame interval, falling back to {@link DEFAULT_INTERVAL}. */
export function interval(value: Time.Micro): Time.Micro {
	return value > 0 ? value : DEFAULT_INTERVAL;
}

/** Frame interval implied by a catalog framerate, or {@link DEFAULT_INTERVAL}. */
export function intervalFromFps(fps: number | undefined): Time.Micro {
	if (fps === undefined || !Number.isFinite(fps) || fps <= 0) return DEFAULT_INTERVAL;
	const value = Time.Micro.fromSecond((1 / fps) as Time.Second);
	return Number.isFinite(value) && value >= 0.0005 && value < 2 ** 64 * 1_000_000 ? value : DEFAULT_INTERVAL;
}

/**
 * Hysteresis around the catalog `stalled` bit.
 *
 * Set once lag exceeds a few frame intervals; clear only after a run of on-time
 * frames. Idle or undemanded broadcasts are never stalled.
 */
export class Detector {
	#on = false;
	#recover = 0;

	/** Whether the rendition is currently stalled. */
	get stalled(): boolean {
		return this.#on;
	}

	/** The catalog value: `true` while stalled, omitted otherwise. */
	flag(): true | undefined {
		return this.#on ? true : undefined;
	}

	/** Feed one sample. Returns whether the catalog flag changed. */
	observe(sample: Sample): boolean {
		if (sample.idle || !sample.demand) return this.#clear();

		const frame = interval(sample.interval);
		const lag = Math.max(sample.mediaLag, sample.quiet) as Time.Micro;
		const over = lag > frame * SET_INTERVALS;
		const onTime = lag <= frame;

		if (!this.#on) {
			if (!over) return false;
			this.#on = true;
			this.#recover = 0;
			return true;
		}

		if (!onTime) {
			this.#recover = 0;
			return false;
		}

		if (!sample.frame) return false;
		this.#recover++;
		if (this.#recover < CLEAR_FRAMES) return false;
		return this.#clear();
	}

	#clear(): boolean {
		this.#recover = 0;
		if (!this.#on) return false;
		this.#on = false;
		return true;
	}
}
