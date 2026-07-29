import { type Effect, Signal } from "@moq/signals";

/**
 * Re-opens a capture when its track dies on its own.
 *
 * A `MediaStreamTrack` ends when the device disappears, the OS revokes it, or another application
 * takes an exclusive device. `getUserMedia` needs no user gesture once permission is granted, so the
 * capture can just be re-opened. Screen capture cannot: `getDisplayMedia` requires a user gesture,
 * so it clears its source instead of using this.
 */
export class Restart {
	// A track that dies instantly on every attempt would spin, so stop after a few tries.
	static readonly LIMIT = 3;

	// How long a track has to survive before earlier failures stop counting against it.
	static readonly SETTLED = 5000;

	readonly #generation = new Signal(0);

	// Deliberately a plain field: effect reruns must not unwind it, or the limit never trips.
	#failures = 0;

	/** Subscribe the capture effect, so a restart reruns it. Call at the top of the run. */
	subscribe(effect: Effect): void {
		effect.get(this.#generation);
	}

	/** Re-open the capture when `track` ends. Call once the track is live. */
	watch(effect: Effect, track: MediaStreamTrack): void {
		effect.timer(() => {
			this.#failures = 0;
		}, Restart.SETTLED);

		effect.event(track, "ended", () => {
			this.#failures += 1;
			if (this.#failures > Restart.LIMIT) {
				console.warn("capture keeps ending, giving up");
				return;
			}

			this.#generation.update((generation) => generation + 1);
		});
	}
}
