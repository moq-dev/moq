import { describe, expect, test } from "bun:test";
import {
	applyKeys,
	canShift,
	changed,
	type DriveTarget,
	longitudinalAxis,
	moveToward,
	REST,
	stepDrive,
	takeoverState,
} from "./input.ts";
import type { DriveState } from "./schemas.ts";

const DT = 1 / 60;

describe("takeoverState", () => {
	test("adopts the vehicle's measured controls", () => {
		const measured: DriveState = { steering: -0.4, throttle: 0.6, brake: 0.2, gear: "reverse" };
		expect(takeoverState(measured)).toEqual(measured);
		expect(takeoverState(measured)).not.toBe(measured);
	});

	test("coasts in forward before the first measured feedback", () => {
		expect(takeoverState(undefined)).toEqual({
			steering: 0,
			throttle: 0,
			brake: 0,
			gear: "forward",
		});
	});
});

function run(state: DriveState, keys: string[] = [], seconds = 1): DriveState {
	const held = new Set(keys);
	let s = { ...state };
	for (let t = 0; t < seconds; t += DT) {
		s = { ...s, ...applyKeys(s, held, DT) };
	}
	return s;
}

describe("applyKeys", () => {
	test("holds everything with no input", () => {
		const s = run({ steering: -0.4, throttle: 0.6, brake: 0.2, gear: "forward" }, [], 5);
		expect(s.steering).toBeCloseTo(-0.4);
		expect(s.throttle).toBeCloseTo(0.6);
		expect(s.brake).toBeCloseTo(0.2);
	});

	test("resting brake never drops without input", () => {
		expect(run({ ...REST }, [], 5).brake).toBe(1);
	});

	test("steering holds its angle when the key is released", () => {
		const turned = run({ ...REST }, ["d"], 1);
		expect(turned.steering).toBeGreaterThan(0.2);
		expect(run(turned, [], 5).steering).toBe(turned.steering);
	});

	test("steering is slew-limited, not instant", () => {
		// A tap responds visibly, while full lock still requires a deliberate hold.
		expect(run({ ...REST }, ["d"], 0.25).steering).toBeGreaterThan(0.1);
		expect(run({ ...REST }, ["d"], 1).steering).toBeLessThan(0.75);
		expect(run({ ...REST }, ["d"], 2).steering).toBeLessThan(1);
		expect(run({ ...REST }, ["d"], 2.5).steering).toBe(1);
	});

	test("C recenters the steering gradually", () => {
		const turned = run({ ...REST }, ["a"], 5);
		expect(turned.steering).toBe(-1);
		const centering = run(turned, ["c"], 1);
		expect(centering.steering).toBeGreaterThan(-1);
		expect(centering.steering).toBeLessThan(0);
		expect(run(turned, ["c"], 5).steering).toBe(0);
	});

	test("opposite steering keys cancel", () => {
		expect(run({ ...REST }, ["a", "d"], 2).steering).toBe(0);
	});

	test("W releases the resting brake promptly, then ramps throttle", () => {
		const firstFrame = applyKeys({ ...REST }, new Set(["w"]), DT);
		expect(firstFrame.brake).toBeGreaterThan(0);
		expect(firstFrame.brake).toBeLessThan(1);
		expect(firstFrame.throttle).toBe(0);

		const early = run({ ...REST }, ["w"], 1);
		expect(early.brake).toBe(0);
		expect(early.throttle).toBeGreaterThan(0.05);
		expect(early.throttle).toBeLessThan(0.2);
		const late = run({ ...REST }, ["w"], 4);
		expect(late.brake).toBe(0);
		expect(late.throttle).toBe(1);
	});

	test("W slews an applied brake off before rebuilding throttle", () => {
		const braking: DriveState = { steering: 0, throttle: 0, brake: 0.6, gear: "forward" };
		const firstFrame = applyKeys(braking, new Set(["w"]), DT);
		expect(firstFrame.brake).toBeGreaterThan(0);
		expect(firstFrame.brake).toBeLessThan(braking.brake);
		expect(firstFrame.throttle).toBe(0);

		let released = braking;
		while (released.brake > 0) {
			released = { ...released, ...applyKeys(released, new Set(["w"]), DT) };
			expect(released.throttle).toBe(0);
		}
		expect(applyKeys(released, new Set(["w"]), DT).throttle).toBeGreaterThan(0);
	});

	test("W drains wheel pedal overlap before releasing the brake", () => {
		const wheel: DriveState = { steering: 0, throttle: 0.6, brake: 0.4, gear: "forward" };
		const firstFrame = applyKeys(wheel, new Set(["w"]), DT);
		expect(firstFrame.throttle).toBeLessThan(wheel.throttle);
		expect(firstFrame.brake).toBe(wheel.brake);

		let normalized = wheel;
		while (normalized.throttle > 0) {
			normalized = { ...normalized, ...applyKeys(normalized, new Set(["w"]), DT) };
			expect(normalized.brake).toBe(wheel.brake);
		}
		const releasing = applyKeys(normalized, new Set(["w"]), DT);
		expect(releasing.throttle).toBe(0);
		expect(releasing.brake).toBeLessThan(wheel.brake);
	});

	test("S cuts throttle before braking, and wins over W", () => {
		const rolling: DriveState = { steering: 0, throttle: 1, brake: 0, gear: "forward" };
		const early = run({ ...rolling }, ["w", "s"], 0.5);
		expect(early.throttle).toBeLessThan(1);
		expect(early.brake).toBe(0);
		const late = run({ ...rolling }, ["w", "s"], 10);
		expect(late.throttle).toBe(0);
		expect(late.brake).toBe(1);
	});
});

describe("longitudinalAxis", () => {
	test("follows brake release before throttle application", () => {
		expect(longitudinalAxis({ throttle: 0.5, brake: 1 }, new Set(["w"]))).toBe("throttle");
		expect(longitudinalAxis({ throttle: 0, brake: 1 }, new Set(["w"]))).toBe("brake");
		expect(longitudinalAxis({ throttle: 0, brake: 0 }, new Set(["w"]))).toBe("throttle");
	});

	test("follows throttle release before brake application", () => {
		expect(longitudinalAxis({ throttle: 0.5, brake: 0 }, new Set(["s"]))).toBe("throttle");
		expect(longitudinalAxis({ throttle: 0, brake: 0 }, new Set(["s"]))).toBe("brake");
	});

	test("matches S winning over W", () => {
		expect(longitudinalAxis({ throttle: 0.5, brake: 0 }, new Set(["w", "s"]))).toBe("throttle");
	});
});

describe("moveToward", () => {
	test("does not overshoot", () => {
		expect(moveToward(0.9, 1, 0.5)).toBe(1);
		expect(moveToward(0.1, 0, 0.5)).toBe(0);
	});
});

describe("changed", () => {
	test("ignores sub-epsilon wiggle", () => {
		expect(changed({ ...REST }, { steering: 0.005, throttle: 0, brake: 1 })).toBe(false);
		expect(changed({ ...REST }, { steering: 0.05, throttle: 0, brake: 1 })).toBe(true);
	});
});

describe("canShift", () => {
	test("requires released throttle and any amount of brake", () => {
		expect(canShift({ throttle: 0, brake: 0.001 })).toBe(true);
		expect(canShift({ throttle: 0.01, brake: 1 })).toBe(true);
		expect(canShift({ throttle: 0.02, brake: 1 })).toBe(false);
		expect(canShift({ throttle: 0, brake: 0 })).toBe(false);
	});
});

describe("stepDrive", () => {
	function target(state: DriveState = { ...REST }) {
		const calls: Array<Partial<DriveState>> = [];
		let cur = state;
		let stopped = false;
		const t: DriveTarget = {
			drive: { peek: () => cur },
			estopped: { peek: () => stopped },
			setDrive(patch) {
				calls.push(patch);
				cur = { ...cur, ...patch };
			},
		};
		return {
			t,
			calls,
			get: () => cur,
			stop: () => {
				stopped = true;
			},
		};
	}

	// One frame of steering at 120 Hz is 0.45/120 = 0.00375, under the publish
	// epsilon, so a loop that skipped sub-epsilon frames would discard every one
	// and held keys would move nothing. Integration must reach the target
	// regardless of size.
	test("commits a frame whose delta is under the publish epsilon", () => {
		const { t, calls, get } = target();
		const dt = 1 / 120;
		const next = applyKeys(get(), new Set(["d"]), dt);
		expect(changed(get(), next)).toBe(false); // below the epsilon, by construction
		stepDrive(t, next);
		expect(calls).toHaveLength(1);
		expect(get().steering).toBeGreaterThan(0);
	});

	test("accumulates a full second of 60 Hz frames", () => {
		const { t, get } = target();
		const dt = 1 / 60;
		for (let i = 0; i < 60; i++) stepDrive(t, applyKeys(get(), new Set(["d"]), dt));
		expect(get().steering).toBeCloseTo(0.45, 2);
	});

	test("auto-selects forward while the truck begins releasing its brake", () => {
		const { t, get } = target();
		stepDrive(t, { steering: 0, throttle: 0, brake: 0.975 });
		expect(get()).toEqual({ steering: 0, throttle: 0, brake: 0.975, gear: "forward" });
	});

	test("does not auto-shift once the truck has fully released the brake", () => {
		const { t, get } = target({ ...REST, brake: 0.1 });
		stepDrive(t, { steering: 0, throttle: 0, brake: 0 });
		expect(get().gear).toBe("neutral");
	});

	test("a latched e-stop swallows the frame", () => {
		const { t, calls, get, stop } = target();
		stop();
		stepDrive(t, applyKeys(get(), new Set(["d"]), 1 / 60));
		expect(calls).toHaveLength(0);
	});
});
