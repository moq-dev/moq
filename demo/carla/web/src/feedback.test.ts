import { describe, expect, test } from "bun:test";
import { COLLISION_ALERT_MS, collisionAge, collisionLabel, DIVERGENCE, diverged, divergence } from "./feedback.ts";
import type { DriveState } from "./schemas.ts";

const REST: DriveState = { steering: 0, throttle: 0, brake: 1, gear: "neutral" };

describe("divergence", () => {
	test("is measured minus requested, per axis", () => {
		const gap = divergence(
			{ steering: 0.5, throttle: 0.4, brake: 0, gear: "forward" },
			{ steering: 0.3, throttle: 0.45, brake: 0, gear: "forward" },
		);
		expect(gap.steering).toBeCloseTo(-0.2);
		expect(gap.throttle).toBeCloseTo(0.05);
		expect(gap.worst).toBeCloseTo(0.2);
		expect(gap.gear).toBe(false);
	});

	test("ignores lag smaller than an actuator always has", () => {
		const gap = divergence(REST, { ...REST, brake: 1 - DIVERGENCE / 2 });
		expect(diverged(gap)).toBe(false);
	});

	test("names only the axes that are actually apart", () => {
		const gap = divergence(
			{ steering: 0.6, throttle: 0.5, brake: 0, gear: "forward" },
			{ steering: 0.1, throttle: 0.49, brake: 0, gear: "forward" },
		);
		expect(diverged(gap)).toBe(true);
	});

	test("calls a truck driving the wrong way a divergence on its own", () => {
		const gap = divergence(
			{ steering: 0, throttle: 0, brake: 0, gear: "forward" },
			{ steering: 0, throttle: 0, brake: 0, gear: "reverse" },
		);
		expect(gap.gear).toBe(true);
		expect(diverged(gap)).toBe(true);
	});

	test("does not fault a neutral request the simulator cannot express", () => {
		// CARLA has no neutral: it holds the direction and applies zero throttle.
		const gap = divergence(REST, { ...REST, gear: "forward" });
		expect(gap.gear).toBe(false);
		expect(diverged(gap)).toBe(false);
	});
});

describe("labels", () => {
	test("scales a collision impulse into kN·s", () => {
		const hit = { at: 1_000_000, other: "vehicle.tesla.model3", impulse: 4210.5 };
		expect(collisionLabel(hit, 0)).toBe("vehicle.tesla.model3 · 4.2 kN·s · just now");
		expect(collisionLabel(hit, COLLISION_ALERT_MS + 4000)).toBe("vehicle.tesla.model3 · 4.2 kN·s · 12s ago");
		expect(collisionLabel({ ...hit, impulse: 640.4 }, 0)).toBe("vehicle.tesla.model3 · 640 N·s · just now");
	});
});

describe("collisionAge", () => {
	const hit = { at: 1_000_000, other: "static.guardrail", impulse: 100 };

	test("ages against the truck's own timestamp, not when this player saw it", () => {
		// A viewer that opens the page three minutes late still sees three
		// minutes, rather than treating the retained snapshot as a fresh crash.
		expect(collisionAge(hit, 1_180_000)).toBe(180_000);
	});

	test("clamps a truck whose clock runs ahead of the viewer's", () => {
		expect(collisionAge(hit, 999_000)).toBe(0);
		expect(collisionLabel(hit, collisionAge(hit, 999_000))).toContain("just now");
	});
});
