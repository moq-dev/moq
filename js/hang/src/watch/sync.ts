import type { Time } from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

export interface SyncProps {
	jitter?: Time.Milli | Signal<Time.Milli>;
	audio?: Time.Milli | Signal<Time.Milli | undefined>;
	video?: Time.Milli | Signal<Time.Milli | undefined>;
}

export class Sync {
	// The earliest time we've received a frame, relative to its timestamp.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	// TODO Update this when RTT changes
	#reference?: Time.Milli;

	// The minimum buffer size, to account for network jitter.
	jitter: Signal<Time.Milli>;

	// Any additional delay required for audio or video.
	audio: Signal<Time.Milli | undefined>;
	video: Signal<Time.Milli | undefined>;

	// The buffer required, based on both audio and video.
	#latency = new Signal<Time.Milli>(0 as Time.Milli);
	readonly latency: Signal<Time.Milli> = this.#latency;

	// A ghetto way to learn when the reference/latency changes.
	// There's probably a way to use Effect, but lets keep it simple for now.
	#update: Promise<void>;
	#resolve!: () => void;

	signals = new Effect();

	constructor(props?: SyncProps) {
		this.jitter = Signal.from(props?.jitter ?? (100 as Time.Milli));
		this.audio = Signal.from(props?.audio);
		this.video = Signal.from(props?.video);

		this.#update = new Promise((resolve) => {
			this.#resolve = resolve;
		});

		this.signals.effect(this.#runLatency.bind(this));
	}

	#runLatency(effect: Effect): void {
		const jitter = effect.get(this.jitter);
		const video = effect.get(this.video) ?? 0;
		const audio = effect.get(this.audio) ?? 0;

		const latency = (Math.max(video, audio) + jitter) as Time.Milli;
		this.#latency.set(latency);

		this.#resolve();

		this.#update = new Promise((resolve) => {
			this.#resolve = resolve;
		});
	}

	// Update the reference if this is the earliest frame we've seen, relative to its timestamp.
	received(timestamp: Time.Milli): void {
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
