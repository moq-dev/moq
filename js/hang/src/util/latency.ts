import type * as Moq from "@moq/lite";
import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

type ConfigWithJitter = { jitter?: number; framerate?: number };

export interface LatencyProps {
	buffer: Signal<Moq.Time.Milli>;
	config: Getter<ConfigWithJitter | undefined>;
}

/**
 * A helper class that computes the final latency based on the catalog's jitter and the user's buffer.
 * If the jitter is not present, then we use framerate to estimate a default.
 *
 * Effective latency = catalog.jitter + buffer
 */
export class Latency {
	buffer: Signal<Moq.Time.Milli>;
	config: Getter<ConfigWithJitter | undefined>;

	signals = new Effect();

	#combined = new Signal<Moq.Time.Milli>(0 as Moq.Time.Milli);
	readonly combined: Signal<Moq.Time.Milli> = this.#combined;

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

	peek(): Moq.Time.Milli {
		return this.#combined.peek();
	}

	close(): void {
		this.signals.close();
	}
}
