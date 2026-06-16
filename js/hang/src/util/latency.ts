import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";

type ConfigWithJitter = { jitter?: number; framerate?: number };

/** Options for constructing a {@link Latency}. */
export interface LatencyProps {
	/** The user-configured buffer, added on top of the catalog jitter. */
	buffer: Signal<Moq.Time.Milli>;
	/** The track config supplying jitter and framerate, or undefined until known. */
	config: Getter<ConfigWithJitter | undefined>;
}

/**
 * A helper class that computes the final latency based on the catalog's jitter and the user's buffer.
 * If the jitter is not present, then we use framerate to estimate a default.
 *
 * Effective latency = catalog.jitter + buffer
 */
export class Latency {
	/** The user-configured buffer, added on top of the catalog jitter. */
	buffer: Signal<Moq.Time.Milli>;
	/** The track config supplying jitter and framerate. */
	config: Getter<ConfigWithJitter | undefined>;

	/** The reactive effect recomputing the combined latency. */
	signals = new Effect();

	#combined = new Signal<Moq.Time.Milli>(0 as Moq.Time.Milli);
	/** The effective latency in milliseconds (catalog jitter plus buffer). */
	readonly combined: Signal<Moq.Time.Milli> = this.#combined;

	/** Start tracking latency from the given buffer and config signals. */
	constructor(props: LatencyProps) {
		this.buffer = props.buffer;
		this.config = props.config;

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const buffer = effect.get(this.buffer);

		// Compute the latency based on the catalog's jitter and the user's buffer.
		const config = effect.get(this.config);

		// Use jitter from catalog if available, otherwise estimate from framerate
		let jitter: number | undefined = config?.jitter;
		if (jitter === undefined && config?.framerate !== undefined && config.framerate > 0) {
			// Estimate jitter as one frame duration if framerate is available
			jitter = 1000 / config.framerate;
		}
		jitter ??= 0;

		const latency = Time.Milli.add(jitter as Moq.Time.Milli, buffer);
		this.#combined.set(latency);
	}

	/** Read the current effective latency without subscribing. */
	peek(): Moq.Time.Milli {
		return this.#combined.peek();
	}

	/** Stop tracking and release the effect. */
	close(): void {
		this.signals.close();
	}
}
