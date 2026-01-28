import type { Time } from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";

export interface SyncProps {
	buffer?: Time.Milli | Signal<Time.Milli>;
	audio?: Catalog.AudioConfig | Signal<Catalog.AudioConfig | undefined>;
	video?: Catalog.VideoConfig | Signal<Catalog.VideoConfig | undefined>;
}

export class Sync {
	// The earliest time we've received a frame, relative to its timestamp.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	// TODO Update this when RTT changes
	#reference?: Time.Milli;

	audio: Signal<Catalog.AudioConfig | undefined>;
	video: Signal<Catalog.VideoConfig | undefined>;

	buffer: Signal<Time.Milli>;

	#latency = new Signal<Time.Milli>(0 as Time.Milli);
	readonly latency: Signal<Time.Milli> = this.#latency;

	// A ghetto way to learn when the reference/latency changes.
	// There's probably a way to use Effect, but lets keep it simple for now.
	#update: Promise<void>;
	#resolve!: () => void;

	signals = new Effect();

	constructor(props?: SyncProps) {
		this.buffer = Signal.from(props?.buffer ?? (100 as Time.Milli));
		this.audio = Signal.from(props?.audio);
		this.video = Signal.from(props?.video);

		this.#update = new Promise((resolve) => {
			this.#resolve = resolve;
		});

		this.signals.effect(this.#runLatency.bind(this));
	}

	#runLatency(effect: Effect): void {
		const buffer = effect.get(this.buffer);

		// Compute the latency based on the catalog's minBuffer and the user's buffer.
		const video = effect.get(this.video);

		// Use minBuffer from catalog if available, otherwise estimate from framerate
		let videoBuffer: number | undefined = video?.minBuffer;
		if (videoBuffer === undefined && video?.framerate !== undefined && video.framerate > 0) {
			// Estimate minBuffer as one frame duration if framerate is available
			videoBuffer = 1000 / video.framerate;
		}
		videoBuffer ??= 0;

		const audio = effect.get(this.audio);
		// TODO if there's no explicit buffer, estimate the audio buffer based on the sample rate and codec?
		const audioBuffer = audio?.minBuffer ?? 0;

		const latency = (Math.max(videoBuffer, audioBuffer) + buffer) as Time.Milli;
		this.#latency.set(latency);

		this.#resolve();

		this.#update = new Promise((resolve) => {
			this.#resolve = resolve;
		});
	}

	// Update the reference if this is the earliest frame we've seen, relative to its timestamp.
	update(timestamp: Time.Milli): void {
		const ref = (performance.now() - timestamp) as Time.Milli;

		if (this.#reference && ref >= this.#reference) {
			return;
		}
		this.#reference = ref;
		this.#resolve();

		this.#update = new Promise((resolve) => {
			this.#resolve = resolve;
		});
	}

	// Sleep until it's time to render this frame.
	//
	// Returns the amount of time we tried to sleep.
	async wait(timestamp: Time.Milli): Promise<void> {
		if (!this.#reference) {
			throw new Error("reference not set; call update() first");
		}

		for (;;) {
			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const now = performance.now();
			const ref = (now - timestamp) as Time.Milli;

			const sleep = this.#reference - ref + this.#latency.peek();
			if (sleep <= 0) return;
			const wait = new Promise((resolve) => setTimeout(resolve, sleep)).then(() => true);

			const ok = await Promise.race([this.#update, wait]);
			if (ok) return;
		}
	}

	close() {
		this.signals.close();
	}
}
