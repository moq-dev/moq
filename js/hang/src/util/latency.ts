import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";

export interface LatencyProps {
	buffer: Signal<Moq.Time.Milli>;
	config: Signal<Catalog.VideoConfig | undefined> | Signal<Catalog.AudioConfig | undefined>;
}

// A helper class that computes the final latency based on the catalog's minBuffer and the user's buffer.
// If the minBuffer is not present, then we use frame timings to compute the frame rate as the default.
export class Latency {
	buffer: Signal<Moq.Time.Milli>;
	config: Signal<Catalog.VideoConfig | undefined> | Signal<Catalog.AudioConfig | undefined>;

	signals = new Effect();

	combined = new Signal<Moq.Time.Milli>(0 as Moq.Time.Milli);

	constructor(props: LatencyProps) {
		this.buffer = props.buffer;
		this.config = props.config;

		this.signals.effect(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const buffer = effect.get(this.buffer);

		// Compute the latency based on the catalog's minBuffer and the user's buffer.
		const config = effect.get(this.config);

		// TODO use the audio frequency + sample_rate?
		// TODO or compute the duration between frames if neither minBuffer nor framerate is set
		let minBuffer: number | undefined = config?.minBuffer;
		if (!minBuffer && config && "framerate" in config) {
			minBuffer = config.framerate ? 1000 / config.framerate : 0;
		}
		minBuffer ??= 0;

		const latency = (minBuffer + buffer) as Moq.Time.Milli;
		this.combined.set(latency);
	}

	close(): void {
		this.signals.close();
	}
}
