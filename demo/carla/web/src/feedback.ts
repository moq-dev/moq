/**
 * Measured feedback: what the vehicle did, against what the driver asked for.
 *
 * The `accepted` state on the status track is command acknowledgement - the
 * gate repeating what it let through. `measured`, `motion`, and `collision` are
 * the vehicle's own response, so everything here is about the gap between the
 * two, which is the only thing a teleoperator can act on.
 *
 * Pure formatting and arithmetic, deliberately: the rendering that uses it
 * (canvas overlay, telemetry rows) is browser-tested, and this is the part with
 * decisions in it.
 *
 * @module
 */
import type { Collision, DriveState } from "./schemas.ts";

/**
 * How far apart the requested and measured axes have to be before the player
 * calls it a divergence.
 *
 * Not a tolerance hiding a bug: an actuator tracking a command always lags it,
 * and a heavy vehicle lags it a lot. Below this the two rails are the same line
 * on screen anyway, so highlighting it would mean the mismatch warning is lit
 * whenever the truck is moving, which is the same as no warning at all.
 */
export const DIVERGENCE = 0.08;

/** One axis of the request/response gap. */
export interface Divergence {
	steering: number;
	throttle: number;
	brake: number;
	/** True when the vehicle is driving in a direction nobody asked for. */
	gear: boolean;
	/** The largest axis error, for deciding whether to say anything at all. */
	worst: number;
}

/** Signed measured-minus-requested error per axis. */
export function divergence(requested: DriveState, measured: DriveState): Divergence {
	const steering = measured.steering - requested.steering;
	const throttle = measured.throttle - requested.throttle;
	const brake = measured.brake - requested.brake;
	// CARLA has no neutral - it drives a neutral request as zero throttle in the
	// direction it was already facing - so a neutral request can never disagree
	// with the direction the physics reports.
	const gear = requested.gear !== "neutral" && measured.gear !== requested.gear;
	return {
		steering,
		throttle,
		brake,
		gear,
		worst: Math.max(Math.abs(steering), Math.abs(throttle), Math.abs(brake)),
	};
}

/** Whether the gap is worth drawing attention to. */
export function diverged(gap: Divergence): boolean {
	return gap.gear || gap.worst >= DIVERGENCE;
}

/** How long a collision stays called out on the tile. */
export const COLLISION_ALERT_MS = 8000;

/**
 * How long ago the collision happened, by the truck's clock against ours.
 *
 * Clamped at zero because the two are clocks on different machines: a truck
 * running slightly ahead would otherwise produce a negative age, and a
 * collision dated in the future reads worse than one that just happened.
 */
export function collisionAge(collision: Collision, now: number): number {
	return Math.max(0, now - collision.at);
}

/** The last collision, aged against the viewer's clock. */
export function collisionLabel(collision: Collision, ageMs: number): string {
	const impulse =
		collision.impulse >= 1000
			? `${(collision.impulse / 1000).toFixed(1)} kN·s`
			: `${Math.round(collision.impulse)} N·s`;
	const age = ageMs < COLLISION_ALERT_MS ? "just now" : `${Math.round(ageMs / 1000)}s ago`;
	return `${collision.other} · ${impulse} · ${age}`;
}
