import { describe, expect, test } from "bun:test";
import { longitudinalIndicator } from "./guides.ts";
import type { DriveState } from "./schemas.ts";

function drive(patch: Partial<DriveState>): DriveState {
	return { steering: 0, throttle: 0, brake: 0, gear: "forward", ...patch };
}

describe("longitudinal indicator", () => {
	test("uses the truck's measured pedal instead of the request", () => {
		const indicator = longitudinalIndicator(drive({ throttle: 0.8 }), drive({ throttle: 0.35 }));
		expect(indicator).toMatchObject({ pedal: "throttle", amount: 0.35, source: "actual" });
	});

	test("points forward throttle away and reverse throttle back", () => {
		const forward = longitudinalIndicator(undefined, drive({ throttle: 0.5 }));
		const reverse = longitudinalIndicator(undefined, drive({ throttle: 0.5, gear: "reverse" }));
		expect(forward?.to).toBeGreaterThan(forward?.from ?? 0);
		expect(reverse?.to).toBeLessThan(reverse?.from ?? 0);
	});

	test("braking points against the selected gear", () => {
		const forward = longitudinalIndicator(undefined, drive({ brake: 0.5 }));
		const reverse = longitudinalIndicator(undefined, drive({ brake: 0.5, gear: "reverse" }));
		expect(forward?.to).toBeLessThan(forward?.from ?? 0);
		expect(reverse?.to).toBeGreaterThan(reverse?.from ?? 0);
	});

	test("shows braking during a brief pedal overlap", () => {
		expect(longitudinalIndicator(undefined, drive({ throttle: 0.7, brake: 0.2 }))).toMatchObject({
			pedal: "brake",
			amount: 0.2,
		});
	});
});
