import { type Effect, Signal } from "@moq/signals";

/**
 * A budget for re-opening a `getUserMedia` capture that failed or died.
 *
 * A `MediaStreamTrack` ends when the device disappears, the OS revokes it, or another application
 * takes an exclusive device, and the reopen that follows can itself fail. Both spend budget, so a
 * device that never works stops being asked.
 *
 * Every outcome reruns the owning effect, including running out of budget. That rerun is what clears
 * `out.source` and stops the stream, via the cleanup the previous run registered, so no caller has to
 * remember to. Screen capture cannot use this: `getDisplayMedia` needs a user gesture to reopen.
 */
export class Retry {
	/** Consecutive failures tolerated before the capture is left alone. */
	static readonly LIMIT = 3;

	/** How long a track has to survive before earlier failures stop counting against it. */
	static readonly SETTLED = 5000;

	readonly #rerun = new Signal(0);

	// Deliberately plain fields: effect reruns must not unwind them, or the budget never runs out.
	#failures = 0;
	#settings: unknown[] | undefined;

	/**
	 * Subscribe the capture effect and report whether an attempt is still worth making.
	 *
	 * False means the budget is spent: return from the run without capturing, and the previous run's
	 * cleanup clears whatever it published.
	 *
	 * The budget belongs to `settings`, the caller's live capture settings. Changing any of them is
	 * new intent rather than another go at the same thing, so it starts a fresh budget: picking a
	 * different device, or fixing a constraint no device could satisfy, revives a spent capture.
	 */
	begin(effect: Effect, settings: unknown[]): boolean {
		effect.get(this.#rerun);

		if (settings.some((setting, i) => setting !== this.#settings?.[i])) {
			this.#settings = settings;
			this.#failures = 0;
		}

		return this.#failures <= Retry.LIMIT;
	}

	/** The attempt produced no usable track. Spends budget and reruns the effect. */
	failed(): void {
		this.#failures += 1;
		this.#rerun.update((rerun) => rerun + 1);
	}

	/** The attempt produced a live track. Reruns the effect if it dies. */
	succeeded(effect: Effect, track: MediaStreamTrack): void {
		effect.timer(() => {
			this.#failures = 0;
		}, Retry.SETTLED);

		effect.event(track, "ended", () => this.failed());
	}

	/** Refund the budget, because something changed that makes another attempt worth trying. */
	refund(): void {
		if (this.#failures === 0) return;

		this.#failures = 0;
		this.#rerun.update((rerun) => rerun + 1);
	}
}
