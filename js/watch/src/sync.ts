import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

export class SyncTrack {
	#jitter = new Signal<Time.Milli | undefined>(undefined);
	readonly jitter: Getter<Time.Milli | undefined> = this.#jitter;
	#onClose: () => void;
	#closed = false;

	constructor(onClose: () => void) {
		this.#onClose = onClose;
	}

	set(jitter: Time.Milli | undefined): void {
		this.#jitter.set(jitter);
	}

	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#onClose();
	}
}

export interface SyncProps {
	jitter?: Time.Milli | Signal<Time.Milli>;
}

export class Sync {
	// The earliest time we've received a frame, relative to its timestamp.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	// TODO Update this when RTT changes
	#reference = new Signal<Time.Milli | undefined>(undefined);
	readonly reference: Signal<Time.Milli | undefined> = this.#reference;

	// The minimum buffer size, to account for network jitter.
	jitter: Signal<Time.Milli>;

	// Dynamic set of track consumers.
	#tracks = new Set<SyncTrack>();
	#tracksVersion = new Signal(0);

	// The buffer required, based on both audio and video.
	#latency = new Signal<Time.Milli>(Time.Milli.zero);
	readonly latency: Signal<Time.Milli> = this.#latency;

	// A ghetto way to learn when the reference/latency changes.
	// There's probably a way to use Effect, but lets keep it simple for now.
	#update: PromiseWithResolvers<void>;

	signals = new Effect();

	constructor(props?: SyncProps) {
		this.jitter = Signal.from(props?.jitter ?? (100 as Time.Milli));

		this.#update = Promise.withResolvers();

		this.signals.run(this.#runLatency.bind(this));
	}

	track(): SyncTrack {
		const t = new SyncTrack(() => {
			this.#tracks.delete(t);
			this.#tracksVersion.update((v) => v + 1);
		});
		this.#tracks.add(t);
		this.#tracksVersion.update((v) => v + 1);
		return t;
	}

	#runLatency(effect: Effect): void {
		const jitter = effect.get(this.jitter);
		effect.get(this.#tracksVersion);

		let max = Time.Milli.zero;
		for (const t of this.#tracks) {
			const v = effect.get(t.jitter) ?? Time.Milli.zero;
			max = Time.Milli.max(max, v);
		}

		const latency = Time.Milli.add(max, jitter);
		this.#latency.set(latency);

		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Update the reference if this is the earliest frame we've seen, relative to its timestamp.
	received(timestamp: Time.Milli): void {
		const ref = Time.Milli.sub(Time.Milli.now(), timestamp);
		const current = this.#reference.peek();

		if (current !== undefined && ref >= current) {
			return;
		}
		this.#reference.set(ref);
		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Sleep until it's time to render this frame.
	async wait(timestamp: Time.Milli): Promise<void> {
		const reference = this.#reference.peek();
		if (reference === undefined) {
			throw new Error("reference not set; call update() first");
		}

		for (;;) {
			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const now = Time.Milli.now();
			const ref = Time.Milli.sub(now, timestamp);

			const currentRef = this.#reference.peek();
			if (currentRef === undefined) return;

			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#latency.peek());
			if (sleep <= 0) return;
			const wait = new Promise((resolve) => setTimeout(resolve, sleep)).then(() => true);

			const ok = await Promise.race([this.#update.promise, wait]);
			if (ok) return;
		}
	}

	close() {
		this.signals.close();
	}
}
