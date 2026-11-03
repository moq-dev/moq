import { describe, expect, test } from "bun:test";
import { REST } from "./input.ts";
import type { DriveState } from "./schemas.ts";
import {
	applyWheel,
	arbitrate,
	Calibration,
	DEFAULT_PROFILE,
	failSafe,
	looksLikeWheel,
	type PadLike,
	pickAxis,
	pickButton,
	readPedal,
	readSteer,
	shortName,
	WHEEL_RATES,
	Wheel,
	type WheelSample,
} from "./wheel.ts";

const DT = 1 / 60;

/** Build a pad snapshot with the given axes and pressed button indices. */
function pad(axes: number[], pressed: number[] = [], id = "Test Wheel", index = 0): PadLike {
	const buttons = Array.from({ length: 8 }, (_, i) => ({ pressed: pressed.includes(i) }));
	return { id, index, axes, buttons, mapping: "", connected: true };
}

/** Chase a wheel target for `seconds`, the way the drive loop does. */
function chase(
	state: DriveState,
	target: { steering: number; throttle: number; brake: number },
	seconds: number,
): DriveState {
	let s = { ...state };
	for (let t = 0; t < seconds; t += DT) {
		s = { ...s, ...applyWheel(s, target, DT) };
	}
	return s;
}

describe("readSteer", () => {
	const map = DEFAULT_PROFILE.steer;

	test("maps the rim straight through", () => {
		expect(readSteer([0], map)).toBe(0);
		expect(readSteer([1], map)).toBeCloseTo(1);
		expect(readSteer([-1], map)).toBeCloseTo(-1);
	});

	test("ignores wobble around center but still reaches full lock", () => {
		expect(readSteer([0.01], map)).toBe(0);
		expect(readSteer([-0.01], map)).toBe(0);
		expect(readSteer([0.5], map)).toBeGreaterThan(0.4);
	});

	test("handles an inverted rim from calibration", () => {
		const inverted = { axis: 0, rest: 0, full: -1 };
		expect(readSteer([-1], inverted)).toBeCloseTo(1);
		expect(readSteer([1], inverted)).toBeCloseTo(-1);
	});

	test("clamps past the calibrated stops", () => {
		expect(readSteer([2], map)).toBeCloseTo(1);
		expect(readSteer([-2], map)).toBeCloseTo(-1);
	});

	test("a missing axis reads as centered rather than NaN", () => {
		expect(readSteer([], map)).toBe(0);
	});
});

describe("readPedal", () => {
	// The common layout: released at +1, floored at -1.
	const map = DEFAULT_PROFILE.throttle;

	test("released is zero and floored is one", () => {
		expect(readPedal([0, 1], map)).toBe(0);
		expect(readPedal([0, -1], map)).toBe(1);
	});

	test("reaches the stops despite deadzone and saturation", () => {
		expect(readPedal([0, 0.97], map)).toBe(0);
		expect(readPedal([0, -0.97], map)).toBe(1);
		expect(readPedal([0, 0], map)).toBeGreaterThan(0.4);
		expect(readPedal([0, 0], map)).toBeLessThan(0.6);
	});

	test("never goes negative or past one", () => {
		expect(readPedal([0, 2], map)).toBe(0);
		expect(readPedal([0, -2], map)).toBe(1);
	});

	test("handles a pedal that rests at zero and rises to one", () => {
		const rising = { axis: 1, rest: 0, full: 1 };
		expect(readPedal([0, 0], rising)).toBe(0);
		expect(readPedal([0, 1], rising)).toBe(1);
	});
});

describe("applyWheel", () => {
	test("tracks the rim absolutely, unlike the hold-style keys", () => {
		const held = chase({ ...REST }, { steering: 0.6, throttle: 0, brake: 1 }, 2);
		expect(held.steering).toBeCloseTo(0.6);
		// Letting the rim return to center returns the steering with it.
		expect(chase(held, { steering: 0, throttle: 0, brake: 1 }, 2).steering).toBeCloseTo(0);
	});

	test("rate-limits a yank without lagging normal steering", () => {
		// One frame cannot cross more than the rate allows.
		const yanked = applyWheel({ ...REST }, { steering: 1, throttle: 0, brake: 1 }, DT);
		expect(yanked.steering).toBeCloseTo(WHEEL_RATES.steer * DT);
		// But a hand-speed sweep is passed through within the same frame.
		const sweep = applyWheel({ ...REST }, { steering: 0.02, throttle: 0, brake: 1 }, DT);
		expect(sweep.steering).toBeCloseTo(0.02);
	});

	test("pedals drive throttle and brake independently", () => {
		const rolling = chase({ ...REST }, { steering: 0, throttle: 0.8, brake: 0 }, 2);
		expect(rolling.throttle).toBeCloseTo(0.8);
		expect(rolling.brake).toBeCloseTo(0);
	});

	test("clamps a miscalibrated sample into range", () => {
		const out = applyWheel({ ...REST }, { steering: 5, throttle: 5, brake: -5 }, 10);
		expect(out.steering).toBe(1);
		expect(out.throttle).toBe(1);
		expect(out.brake).toBe(0);
	});
});

/** A wheel reading, defaulting to centered and at rest. */
function sample(patch: Partial<WheelSample> = {}): WheelSample {
	return {
		steering: 0,
		throttle: 0,
		brake: 0,
		pressed: new Set(),
		moved: false,
		...patch,
	};
}

describe("arbitrate", () => {
	test("a wheel sitting still does not take control from the keyboard", () => {
		expect(arbitrate("keyboard", sample()).source).toBe("keyboard");
	});

	test("moving the wheel takes control", () => {
		expect(arbitrate("keyboard", sample({ steering: 0.3, moved: true })).source).toBe("wheel");
	});

	test("a depressed throttle cannot be what hands over control", () => {
		// A foot resting on the pedal while the rim is bumped must not arm it.
		const armed = arbitrate("keyboard", sample({ steering: 0.3, throttle: 0.5, moved: true }));
		expect(armed.source).toBe("keyboard");
		// Releasing the pedal and moving again does.
		expect(arbitrate("keyboard", sample({ steering: 0.4, moved: true })).source).toBe("wheel");
	});

	test("an armed wheel keeps control while it sits still", () => {
		expect(arbitrate("wheel", sample({ steering: 0.5 })).source).toBe("wheel");
		// Including with the throttle down - the arming rule is for handover only.
		expect(arbitrate("wheel", sample({ throttle: 0.8 })).source).toBe("wheel");
	});

	test("losing an armed wheel falls back to the keyboard and reports the loss", () => {
		expect(arbitrate("wheel", undefined)).toEqual({ source: "keyboard", lost: true });
	});

	test("losing a wheel that was never armed is not a loss to act on", () => {
		expect(arbitrate("keyboard", undefined)).toEqual({ source: "keyboard", lost: false });
	});

	test("the loss only fires on the transition, not every tick after", () => {
		let src = arbitrate("wheel", undefined);
		expect(src.lost).toBe(true);
		src = arbitrate(src.source, undefined);
		expect(src.lost).toBe(false);
	});
});

describe("failSafe", () => {
	test("cuts the throttle and brakes when the wheel vanishes mid-drive", () => {
		const rolling: DriveState = { steering: 0.4, throttle: 0.9, brake: 0, gear: "forward" };
		const safe = failSafe(rolling);
		expect(safe.throttle).toBe(0);
		expect(safe.brake).toBe(1);
	});

	test("holds steering rather than centering it", () => {
		// Unplugging mid-corner must not trade a runaway for a swerve.
		const cornering: DriveState = { steering: -0.7, throttle: 0.5, brake: 0, gear: "forward" };
		expect(failSafe(cornering).steering).toBe(-0.7);
		expect(failSafe(cornering).steering).not.toBe(REST.steering);
	});

	test("is idempotent, so a repeat cannot make things worse", () => {
		const rolling: DriveState = { steering: 0.4, throttle: 0.9, brake: 0, gear: "forward" };
		const once = { ...rolling, ...failSafe(rolling) };
		expect(failSafe(once)).toEqual(failSafe(rolling));
	});
});

describe("looksLikeWheel", () => {
	test("matches known wheels by name", () => {
		expect(looksLikeWheel(pad([0, 1, 1], [], "Logitech G29 Driving Force Racing Wheel"))).toBe(true);
		expect(looksLikeWheel(pad([0, 1, 1], [], "Thrustmaster T300RS"))).toBe(true);
	});

	test("accepts an unfamiliar unmapped device with enough axes", () => {
		expect(looksLikeWheel(pad([0, 1, 1], [], "Unknown HID device"))).toBe(true);
	});

	test("rejects a standard gamepad", () => {
		const gamepad: PadLike = { ...pad([0, 0, 0, 0], [], "Xbox Controller"), mapping: "standard" };
		expect(looksLikeWheel(gamepad)).toBe(false);
	});
});

describe("pickAxis", () => {
	test("picks the axis that moved furthest, not the first that moved", () => {
		// Turning the rim cross-talks into the pedal axes a little.
		expect(pickAxis([0, 1, 1], [0.3, 0.2, 1], new Set())).toBe(1);
	});

	test("ignores axes already bound to another control", () => {
		expect(pickAxis([0, 1, 1], [0.3, 0.2, 1], new Set([1]))).toBe(0);
	});

	test("returns nothing when the driver didn't move enough", () => {
		expect(pickAxis([0, 1, 1], [0.05, 0.98, 1], new Set())).toBeUndefined();
	});
});

describe("pickButton", () => {
	test("finds a newly pressed button", () => {
		expect(pickButton([false, false], [false, true])).toBe(1);
	});

	test("ignores buttons already held at the baseline", () => {
		expect(pickButton([true, false], [true, false])).toBeUndefined();
	});

	test("ignores buttons already bound", () => {
		expect(pickButton([false, false], [true, true], new Set([0]))).toBe(1);
	});
});

describe("Calibration", () => {
	/** Walk the axis half of the sequence for a wheel with inverted pedals. */
	function walkAxes(cal: Calibration) {
		cal.update(pad([0, 1, 1]));
		expect(cal.capture()).toBe(true); // rest
		cal.update(pad([1, 1, 1]));
		expect(cal.capture()).toBe(true); // full right lock
		cal.update(pad([0, -1, 1]));
		expect(cal.capture()).toBe(true); // throttle floored
		cal.update(pad([0, 1, -1]));
		expect(cal.capture()).toBe(true); // brake floored
	}

	test("learns the axis layout from the driver's movements", () => {
		const cal = new Calibration();
		walkAxes(cal);
		expect(cal.skip()).toBe(true); // gear up
		expect(cal.skip()).toBe(true); // gear down
		expect(cal.skip()).toBe(true); // e-stop
		expect(cal.done).toBe(true);

		const profile = cal.result();
		expect(profile).toBeDefined();
		if (!profile) return;
		expect(profile.steer).toEqual({ axis: 0, rest: 0, full: 1 });
		expect(profile.throttle).toEqual({ axis: 1, rest: 1, full: -1 });
		expect(profile.brake).toEqual({ axis: 2, rest: 1, full: -1 });
		expect(profile.gearUp).toBeUndefined();

		// And the learned profile reads back the way it was demonstrated.
		expect(readSteer([1, 1, 1], profile.steer)).toBeCloseTo(1);
		expect(readPedal([0, -1, 1], profile.throttle)).toBe(1);
		expect(readPedal([0, 1, 1], profile.brake)).toBe(0);
	});

	test("a control that barely moves is rejected, leaving the step up", () => {
		const cal = new Calibration();
		cal.update(pad([0, 1, 1]));
		cal.capture();
		const step = cal.step;
		cal.update(pad([0.05, 1, 1]));
		expect(cal.capture()).toBe(false);
		expect(cal.step).toBe(step);
	});

	test("required steps cannot be skipped", () => {
		const cal = new Calibration();
		expect(cal.skip()).toBe(false);
		expect(cal.index).toBe(0);
	});

	test("buttons bind on the press itself", () => {
		const cal = new Calibration();
		walkAxes(cal);
		cal.update(pad([0, 1, 1], [4]));
		expect(cal.index).toBe(5);
		cal.update(pad([0, 1, 1], [4, 5]));
		expect(cal.index).toBe(6);
		expect(cal.skip()).toBe(true);

		const profile = cal.result();
		expect(profile?.gearUp).toBe(4);
		expect(profile?.gearDown).toBe(5);
		expect(profile?.estop).toBeUndefined();
	});

	test("a button held across a step boundary does not bind twice", () => {
		const cal = new Calibration();
		walkAxes(cal);
		cal.update(pad([0, 1, 1], [4])); // binds gear up
		cal.update(pad([0, 1, 1], [4])); // still held: must not bind gear down
		expect(cal.result()).toBeUndefined();
		expect(cal.index).toBe(5);
	});

	test("an incomplete walk yields no profile", () => {
		const cal = new Calibration();
		cal.update(pad([0, 1, 1]));
		cal.capture();
		expect(cal.result()).toBeUndefined();
	});
});

describe("Wheel button edges", () => {
	/** Drive a Wheel against a scripted sequence of pad states. */
	function polling(states: PadLike[]): (WheelSample | undefined)[] {
		const pads = [...states];
		const original = navigator.getGamepads;
		// @ts-expect-error - stubbing the Gamepad API for the duration of the test.
		navigator.getGamepads = () => [pads.shift() ?? null];
		try {
			const wheel = new Wheel();
			return states.map(() => wheel.poll());
		} finally {
			navigator.getGamepads = original;
		}
	}

	test("a paddle already held at connect is not a press", () => {
		// Plugging in while resting a hand on the shift paddle must not shift.
		const [first, second] = polling([
			pad([0, 1, 1], [4]), // adopted with the paddle already down
			pad([0, 1, 1], [4]), // still down
		]);
		expect(first?.pressed.has("gearUp")).toBe(false);
		expect(second?.pressed.has("gearUp")).toBe(false);
	});

	test("releasing and pressing that same paddle does register", () => {
		const samples = polling([pad([0, 1, 1], [4]), pad([0, 1, 1], []), pad([0, 1, 1], [4])]);
		expect(samples[2]?.pressed.has("gearUp")).toBe(true);
	});

	test("a press is an edge, not a level", () => {
		const samples = polling([pad([0, 1, 1], []), pad([0, 1, 1], [4]), pad([0, 1, 1], [4])]);
		expect(samples[1]?.pressed.has("gearUp")).toBe(true);
		expect(samples[2]?.pressed.has("gearUp")).toBe(false);
	});

	test("connecting does not report movement", () => {
		const [first] = polling([pad([0.5, 1, 1]), pad([0.5, 1, 1])]);
		expect(first?.moved).toBe(false);
	});

	test("reports loss before adopting a different connected wheel", () => {
		const first = pad([0, 1, 1], [], "Wheel A", 0);
		const replacement = pad([0.8, -1, 1], [], "Wheel B", 1);
		const snapshots: (PadLike | null)[][] = [[first, replacement], [replacement], [replacement]];
		const original = navigator.getGamepads;
		// @ts-expect-error - stubbing the Gamepad API for the duration of the test.
		navigator.getGamepads = () => snapshots.shift() ?? [];
		try {
			const wheel = new Wheel();
			expect(wheel.poll()).toBeDefined();
			expect(wheel.id).toBe("Wheel A");
			expect(wheel.poll()).toBeUndefined();
			expect(wheel.id).toBeUndefined();
			const adopted = wheel.poll();
			expect(wheel.id).toBe("Wheel B");
			expect(adopted?.moved).toBe(false);
		} finally {
			navigator.getGamepads = original;
		}
	});
});

describe("shortName", () => {
	test("strips the vendor/product noise browsers append", () => {
		expect(shortName("Logitech G29 (Vendor: 046d Product: c24f)")).toBe("Logitech G29");
		expect(shortName("Plain Wheel")).toBe("Plain Wheel");
	});
});
