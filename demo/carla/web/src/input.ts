/**
 * Pure keyboard-to-drive-state helpers.
 *
 * The simulated vehicles are heavy trucks: inputs are deliberate, slew-limited
 * nudges, and axes **hold** their position when the keys are released - steering
 * does not snap back to center and the throttle does not drop to zero. The only
 * instant transition is e-stop (handled elsewhere), which latches into REST.
 *
 * @module
 */

import type { DriveState } from "./schemas.ts";

/** The truck's safe resting state. */
export const REST: DriveState = { steering: 0, throttle: 0, brake: 1, gear: "neutral" };

/** Initial manual handoff: preserve live feedback, or coast when none exists. */
export function takeoverState(measured: DriveState | undefined): DriveState {
	return measured ? { ...measured } : { steering: 0, throttle: 0, brake: 0, gear: "forward" };
}

/** Slew rates in normalized units per second. Heavy responses, with fast safety actions. */
const RATES = {
	/** Steering: full lock takes ~2.2 s of held key. */
	steer: 0.45,
	/** Recentering (C) is a touch quicker than turning further. */
	steerCenter: 0.65,
	/** Throttle takes 2.5 s to build from idle to full. */
	throttleUp: 0.4,
	/** ...and backs off faster than it builds. */
	throttleDown: 1.2,
	/** Brake release is deliberate: full to clear takes ~0.67 s. */
	brakeDown: 1.5,
	/** Brakes apply quickly (safety), */
	brakeUp: 1.5,
};

/** A gear change requires some brake and a released throttle. */
const SHIFT_MAX_THROTTLE = 0.01;

/** Mirror the truck-side gear interlock so rejected shifts get immediate UI feedback. */
export function canShift(state: Pick<DriveState, "throttle" | "brake">): boolean {
	return state.brake > 0 && state.throttle <= SHIFT_MAX_THROTTLE;
}

/** Move `current` toward `target` by at most `step`, without overshooting. */
export function moveToward(current: number, target: number, step: number): number {
	if (Math.abs(target - current) <= step) return target;
	return current + Math.sign(target - current) * step;
}

/**
 * Integrate the held keys into the next analog state over `dt` seconds.
 *
 * - `A`/`D` (or arrows) slew the steering toward full lock; releasing holds
 *   the current angle, and `C` slews it back to center.
 * - `W` first drains any throttle/brake overlap from a wheel, then releases the
 *   brake promptly and builds throttle.
 * - `S` first eases the throttle off, then applies brake (so it always wins
 *   over `W`).
 * - No keys: everything holds where it is.
 */
export function applyKeys(
	current: DriveState,
	held: ReadonlySet<string>,
	dt: number,
): Pick<DriveState, "steering" | "throttle" | "brake"> {
	let { steering, throttle, brake } = current;

	const left = held.has("a") || held.has("arrowleft");
	const right = held.has("d") || held.has("arrowright");
	const accelerating = held.has("w") || held.has("arrowup");
	const braking = held.has("s") || held.has("arrowdown");

	if (left !== right) {
		const dir = right ? 1 : -1;
		steering = Math.max(-1, Math.min(1, steering + dir * RATES.steer * dt));
	} else if (held.has("c")) {
		steering = moveToward(steering, 0, RATES.steerCenter * dt);
	}

	if (braking) {
		// Ease off the throttle before touching the brakes.
		if (throttle > 0) {
			throttle = moveToward(throttle, 0, RATES.throttleDown * dt);
		} else {
			brake = moveToward(brake, 1, RATES.brakeUp * dt);
		}
	} else if (accelerating) {
		// A wheel can apply both pedals independently. Drain that overlap before
		// releasing the brake, then finish braking before building throttle.
		if (throttle > 0 && brake > 0) {
			throttle = moveToward(throttle, 0, RATES.throttleDown * dt);
		} else if (brake > 0) {
			brake = moveToward(brake, 0, RATES.brakeDown * dt);
		} else {
			throttle = moveToward(throttle, 1, RATES.throttleUp * dt);
		}
	}

	return { steering, throttle, brake };
}

/** The longitudinal axis the held keys will move next, matching {@link applyKeys}' phase ordering. */
export function longitudinalAxis(
	current: Pick<DriveState, "throttle" | "brake">,
	held: ReadonlySet<string>,
): "throttle" | "brake" | undefined {
	const accelerating = held.has("w") || held.has("arrowup");
	const braking = held.has("s") || held.has("arrowdown");
	if (braking) return current.throttle > 0 ? "throttle" : "brake";
	if (accelerating) {
		if (current.throttle > 0 && current.brake > 0) return "throttle";
		return current.brake > 0 ? "brake" : "throttle";
	}
	return undefined;
}

/** Return whether an analog state differs enough to publish. */
export function changed(current: DriveState, next: Pick<DriveState, "steering" | "throttle" | "brake">): boolean {
	const epsilon = 0.01;
	return (
		Math.abs(current.steering - next.steering) > epsilon ||
		Math.abs(current.throttle - next.throttle) > epsilon ||
		Math.abs(current.brake - next.brake) > epsilon
	);
}

/** The subset of `Truck` the control loop drives, so the loop is testable without one. */
export interface DriveTarget {
	readonly drive: { peek(): DriveState };
	readonly estopped: { peek(): boolean };
	setDrive(patch: Partial<DriveState>): void;
}

/**
 * One frame of the control loop.
 *
 * Hands the integrated state to the target UNCONDITIONALLY. Gating this on a
 * per-frame delta would freeze steering: at 120 Hz, one frame moves 0.45/120 =
 * 0.00375, under the publish epsilon, so every frame would be discarded.
 * Throttling belongs on the wire (see `Truck.setDrive`), which measures against
 * the last state actually sent. The truck shifts itself into forward on the
 * first brake-release frame, while the brake is still applied, so W/pedal
 * acceleration works without bypassing the gear interlock.
 */
export function stepDrive(target: DriveTarget, next: Pick<DriveState, "steering" | "throttle" | "brake">): void {
	if (target.estopped.peek()) return;
	const current = target.drive.peek();
	const requested = { ...current, ...next };
	const gear =
		current.gear === "neutral" && next.brake < current.brake && canShift(requested) ? "forward" : undefined;
	target.setDrive(gear ? { ...next, gear } : next);
}
