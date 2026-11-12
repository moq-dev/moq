import { Duration } from "@moq/clock";
import { type Effect, Signal } from "@moq/signals";

/**
 * A budget for re-opening a `getUserMedia` capture that failed or died.
 *
 * A `MediaStreamTrack` ends when the device disappears, the OS revokes it, or another application
 * takes an exclusive device, and the reopen that follows can itself fail. Transient open failures
 * and ended tracks spend budget; other open failures are terminal immediately.
 *
 * Every outcome reruns the owning effect, including running out of budget. That rerun is what clears
 * `out.source` and stops the stream, via the cleanup the previous run registered, so no caller has to
 * remember to. The same rerun is when {@link spent} holds the last error; the caller assigns it to
 * `out.error`. A backoff rerun leaves it empty, so a retry stays quiet. Screen capture cannot use
 * this: `getDisplayMedia` needs a user gesture to reopen.
 */
export class Retry {
	/** Consecutive failures tolerated before the capture is left alone. */
	static readonly LIMIT = 3;

	/** How long a track has to survive before earlier failures stop counting against it. */
	static readonly SETTLED = 5000;

	/**
	 * Escalating pause between attempts, so a device mid-re-enumerate gets a moment rather than
	 * being asked three more times in the same millisecond.
	 *
	 * No give-up deadline: {@link LIMIT} is this budget, counted in attempts. Counting time instead
	 * would make the outcome depend on how long the OS takes to say no.
	 */
	static readonly DELAY = { initial: 250, multiplier: 2, max: 1000 };

	readonly #rerun = new Signal(0);

	// Deliberately plain fields: effect reruns must not unwind them, or the budget never runs out.
	#settings: unknown[] | undefined;
	#state: State = fresh();

	/**
	 * Subscribe the capture effect and report whether an attempt is worth making right now.
	 *
	 * False means don't capture on this run: either the budget is spent, or the backoff from the
	 * last failure hasn't elapsed and a rerun is already scheduled for when it has. Return from the
	 * run either way, and the previous run's cleanup clears whatever it published. Splitting it like
	 * this is what lets a dead track be dropped immediately while the *reopen* still waits.
	 *
	 * The budget belongs to `settings`, the caller's live capture settings. Changing any of them is
	 * new intent rather than another go at the same thing, so it starts a fresh budget: picking a
	 * different device, or fixing a constraint no device could satisfy, revives a spent capture.
	 */
	begin(effect: Effect, settings: unknown[]): boolean {
		effect.get(this.#rerun);

		if (settings.some((setting, i) => setting !== this.#settings?.[i])) {
			this.#settings = settings;
			this.#state = fresh();
		}

		const state = this.#state;
		if (state.kind === "spent") return false;

		const wait = state.wait;
		if (wait !== undefined) {
			this.#state = { ...state, wait: undefined };
			effect.timer(() => this.#rerun.update((rerun) => rerun + 1), Duration.millis(wait));
			return false;
		}

		return true;
	}

	/** Spends budget and reruns the effect, returning whether another attempt is allowed. */
	failed(error: Error): boolean {
		const state = this.#state;
		if (state.kind === "trying" && state.failures < Retry.LIMIT) {
			// Equal jitter, so a page with several captures doesn't reopen them all on the same tick.
			const wait = state.delay * (0.5 + Math.random() / 2);
			const delay = Math.min(state.delay * Retry.DELAY.multiplier, Retry.DELAY.max);
			this.#state = { kind: "trying", failures: state.failures + 1, delay, wait };
		} else {
			this.#state = { kind: "spent", error };
		}
		this.#rerun.update((rerun) => rerun + 1);
		return this.#state.kind === "trying";
	}

	/**
	 * The error from the attempt that spent the budget.
	 * Undefined while another attempt is allowed, including during backoff.
	 */
	spent(): Error | undefined {
		return this.#state.kind === "spent" ? this.#state.error : undefined;
	}

	/** Spend budget because the track ended. Reported via {@link spent} once nothing remains. */
	ended(): boolean {
		return this.failed(new DOMException("The media track ended", "AbortError"));
	}

	/**
	 * Classify a `getUserMedia` rejection, returning whether another attempt is allowed.
	 *
	 * Only a busy device (`NotReadableError`) or an aborted request (`AbortError`) spends budget;
	 * anything else, including bad constraints, is terminal at once.
	 */
	rejected(error: unknown): boolean {
		const cause = error instanceof Error ? error : new Error(String(error));
		if (cause.name === "NotReadableError" || cause.name === "AbortError") {
			return this.failed(cause);
		}
		// Stop attempting this capture until its settings, device list, or permission changes.
		this.#state = { kind: "spent", error: cause };
		this.#rerun.update((rerun) => rerun + 1);
		return false;
	}

	/** The attempt produced a live track. Reruns the effect if it dies. */
	succeeded(effect: Effect, track: MediaStreamTrack): void {
		effect.timer(() => {
			this.#state = fresh();
		}, Duration.millis(Retry.SETTLED));

		effect.event(track, "ended", () => this.ended());
	}

	/** Refund the budget, because something changed that makes another attempt worth trying. */
	refund(): void {
		const state = this.#state;
		if (state.kind === "trying" && state.failures === 0 && state.wait === undefined) return;

		this.#state = fresh();
		this.#rerun.update((rerun) => rerun + 1);
	}
}

// Where a capture's budget stands. Its cause is kept only once spent, so a retry stays quiet.
type State =
	// Attempts allowed: the failures so far, the next backoff, and the pause the next attempt still
	// owes, set by `failed` and paid by `begin`.
	| { kind: "trying"; failures: number; delay: number; wait?: DOMHighResTimeStamp }
	// Out of budget until the settings, device list, or permission change.
	| { kind: "spent"; error: Error };

// No failures, and none of the pause they earn.
function fresh(): State {
	return { kind: "trying", failures: 0, delay: Retry.DELAY.initial };
}
