/**
 * Shared rate control for media senders, mirroring `moq_mux::rate`.
 *
 * {@link Control} turns each bandwidth grant into the target bitrate a sender should produce.
 * {@link Rung} adds the cutoff for one rendition of several: below it the rendition stops and viewers
 * fall back to a lower one. Same rule and constants as the Rust module, so every publisher backs off
 * the same way.
 *
 * @module
 */
import { Instant, Nano } from "@moq/clock";

// Ignore moves smaller than this fraction of the current target, so a jittering estimate doesn't
// reconfigure the sender on every sample.
const HYSTERESIS = 0.05;

// How fast the target may climb back, as a fraction of the current target per second. Drops apply at
// once: overshooting a closing uplink stalls media, undershooting an opening one only costs quality.
const RAMP = 0.25;

/** How a bandwidth grant maps onto the bitrate a sender produces, in bits per second. */
export interface Policy {
	/** Upper bound, normally the configured bitrate. A grant only ever takes the target down from here. */
	max: number;
	/** Lower bound: below it the target holds and the transport sheds the excess. */
	min: number;
}

/** A policy targeting at most `max`, with a floor a tenth of it. */
export function policy(max: number): Policy {
	return { max, min: Math.floor(max / 10) };
}

/** Maps bandwidth grants onto a target bitrate, per a {@link Policy}. */
export class Control {
	readonly policy: Policy;
	#target: number;
	// When the target last moved, anchoring the ramp. Undefined until the first change.
	#applied: Instant | undefined;

	/** Start at {@link Policy.max}: until a grant says otherwise, send what was configured. */
	constructor(policy: Policy) {
		this.policy = policy;
		this.#target = policy.max;
	}

	/** The current target bitrate. */
	get target(): number {
		return this.#target;
	}

	/**
	 * Feed a new grant observed at `now`, returning the new target when it moved enough to be worth
	 * applying. An undefined grant holds the current target: losing the estimate is not evidence the
	 * uplink got better.
	 */
	update(estimate: number | undefined, now: Instant): number | undefined {
		if (estimate === undefined) return undefined;

		const min = Math.min(this.policy.min, this.policy.max);
		const desired = Math.min(Math.max(estimate, min), this.policy.max);

		let next: number;
		if (desired <= this.#target || this.#applied === undefined) {
			next = desired;
		} else {
			// Suppressed raises leave the anchor alone, so the allowance keeps growing until one clears
			// the deadband instead of being starved by a per-tick allowance smaller than it.
			const elapsed = Nano.toSecond(Instant.durationSince(now, this.#applied));
			const grown = Math.floor(this.#target * (1 + RAMP * elapsed));
			next = Math.min(Math.max(Math.min(grown, desired), min), this.policy.max);
		}

		// The last step of a recovery onto the configured ceiling is exempt, or a target within the
		// deadband of it would never get back. Drops keep the deadband.
		const settling = next > this.#target && next === this.policy.max;
		if (!settling && Math.abs(next - this.#target) < Math.floor(this.#target * HYSTERESIS)) {
			return undefined;
		}

		this.#target = next;
		this.#applied = now;
		return next;
	}

	// Restart at `target` as of `now`, for a rung turning back on.
	restart(target: number, now: Instant): void {
		this.#target = target;
		this.#applied = now;
	}
}

/** What a {@link Rung} should be doing: encoding at a bitrate, or stopped and advertised disabled. */
export type Target = { enabled: true; bitrate: number } | { enabled: false };

/**
 * Rate control for one rendition of a source encoded at several bitrates.
 *
 * A {@link Control} that also turns the rendition off once its grant falls below its band, the point
 * where the next lower rendition serves viewers better than a starved copy of this one. It turns back
 * on once the grant clears the cutoff by the hysteresis, restarting at the cutoff and ramping. Feed it
 * the grant of an idle reservation too: once viewers leave a disabled rendition, nothing else would
 * bring it back.
 */
export class Rung {
	readonly #control: Control;
	readonly #cutoff: number;
	#enabled = true;

	/**
	 * A rendition per `policy`, above a rendition sending at most `lower`. The cutoff is
	 * `(max + 2 * lower) / 3`; pass zero for the lowest rendition to turn it off below a third of its
	 * maximum. Throws when `lower` is not below the maximum, which leaves no band to encode in.
	 */
	constructor(policy: Policy, lower: number) {
		if (!Number.isFinite(lower) || lower < 0 || lower >= policy.max) {
			throw new Error(`lower rendition (${lower}) must be below this one's maximum (${policy.max})`);
		}
		this.#control = new Control(policy);
		this.#cutoff = Math.floor((policy.max + 2 * lower) / 3);
	}

	/** What the rendition should be doing right now. */
	get target(): Target {
		return this.#enabled ? { enabled: true, bitrate: this.#control.target } : { enabled: false };
	}

	/** Feed a new grant, returning the new target when it changed enough to apply. See {@link Control.update}. */
	update(estimate: number | undefined, now: Instant): Target | undefined {
		if (!this.#enabled) {
			if (estimate === undefined) return undefined;
			const max = this.#control.policy.max;
			// Capped at the ceiling, or a cutoff within the hysteresis of it could never be cleared.
			const resume = Math.min(Math.floor(this.#cutoff * (1 + HYSTERESIS)), max);
			if (Math.min(estimate, max) < resume) return undefined;

			// Restart low and ramp: the estimate was measured while this rendition sent nothing.
			this.#enabled = true;
			this.#control.restart(this.#cutoff, now);
			return this.target;
		}

		const next = this.#control.update(estimate, now);
		if (next === undefined) return undefined;
		if (next < this.#cutoff) this.#enabled = false;
		return this.target;
	}
}
