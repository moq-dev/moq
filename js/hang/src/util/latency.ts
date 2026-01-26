import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

type ConfigWithMinBuffer = { minBuffer?: number; framerate?: number };

export interface LatencyProps {
	buffer: Signal<Moq.Time.Milli>;
	config: Getter<ConfigWithMinBuffer | undefined>;
}

/**
 * A helper class that computes the final latency based on the catalog's minBuffer and the user's buffer.
 * If the minBuffer is not present, then we use framerate to estimate a default.
 *
 * Effective latency = catalog.minBuffer + buffer
 */
export class Latency {
	buffer: Signal<Moq.Time.Milli>;
	config: Getter<ConfigWithMinBuffer | undefined>;

	signals = new Effect();

	#combined = new Signal<Moq.Time.Milli>(0 as Moq.Time.Milli);
	readonly combined: Signal<Moq.Time.Milli> = this.#combined;

	constructor(props: LatencyProps) {
		this.buffer = props.buffer;
		this.config = props.config;

		this.signals.effect(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const buffer = effect.get(this.buffer);

		// Compute the latency based on the catalog's minBuffer and the user's buffer.
		const config = effect.get(this.config);

		// Use minBuffer from catalog if available, otherwise estimate from framerate
		let minBuffer: number | undefined = config?.minBuffer;
		if (minBuffer === undefined && config?.framerate !== undefined && config.framerate > 0) {
			// Estimate minBuffer as one frame duration if framerate is available
			minBuffer = 1000 / config.framerate;
		}
		minBuffer ??= 0;

		const latency = (minBuffer + buffer) as Moq.Time.Milli;
		this.#combined.set(latency);
	}

	peek(): Moq.Time.Milli {
		return this.#combined.peek();
	}

	close(): void {
		this.signals.close();
	}
}
