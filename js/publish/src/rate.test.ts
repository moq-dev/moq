import { expect, test } from "bun:test";
import { Instant, Milli, Nano } from "@moq/clock";
import { Control, policy, Rung } from "./rate";

const at = (ms: number) => Instant(Nano.fromMilli(Milli(ms)));

// 5 Mbps above a 2.5 Mbps rendition, so the cutoff lands on 3_333_333.
function rung(): Rung {
	return new Rung(policy(5_000_000), 2_500_000);
}

test("a drop applies at once and a raise ramps", () => {
	const control = new Control(policy(4_000_000));
	expect(control.update(1_000_000, at(0))).toBe(1_000_000);
	// 25%/s: one second later only 1M -> 1.25M.
	expect(control.update(4_000_000, at(1000))).toBe(1_250_000);
});

test("a missing estimate holds the target", () => {
	const control = new Control(policy(4_000_000));
	control.update(2_000_000, at(0));
	expect(control.update(undefined, at(10_000))).toBeUndefined();
	expect(control.target).toBe(2_000_000);
});

test("suppressed raises do not starve the ramp", () => {
	const control = new Control(policy(4_000_000));
	control.update(1_000_000, at(0));
	// 2.5% per 100ms tick is under the 5% deadband; the second tick clears it.
	expect(control.update(4_000_000, at(100))).toBeUndefined();
	expect(control.update(4_000_000, at(200))).toBe(1_050_000);
});

test("a raise reaching the ceiling beats the deadband", () => {
	const control = new Control(policy(4_000_000));
	control.update(1_000_000, at(0));
	let last: number | undefined;
	for (let tick = 1; tick <= 200; tick++) {
		last = control.update(4_000_000, at(100 * tick)) ?? last;
	}
	expect(last).toBe(4_000_000);
});

test("a rung disables below its cutoff", () => {
	const r = rung();
	expect(r.target).toEqual({ enabled: true, bitrate: 5_000_000 });
	expect(r.update(3_500_000, at(0))).toEqual({ enabled: true, bitrate: 3_500_000 });
	expect(r.update(3_000_000, at(100))).toEqual({ enabled: false });
});

test("the lowest rung disables below a third", () => {
	const r = new Rung(policy(350_000), 0);
	expect(r.update(120_000, at(0))).toEqual({ enabled: true, bitrate: 120_000 });
	expect(r.update(110_000, at(100))).toEqual({ enabled: false });
});

test("a rung resumes past the hysteresis and ramps", () => {
	const r = rung();
	r.update(3_000_000, at(0));
	expect(r.update(3_400_000, at(1000))).toBeUndefined();
	expect(r.update(3_499_000, at(2000))).toBeUndefined();

	// Resume at the cutoff rather than the whole estimate, then ramp at 25%/s.
	expect(r.update(5_000_000, at(3000))).toEqual({ enabled: true, bitrate: 3_333_333 });
	expect(r.update(5_000_000, at(4000))).toEqual({ enabled: true, bitrate: Math.floor(3_333_333 * 1.25) });
});

test("jitter at the cutoff does not flap", () => {
	const r = rung();
	r.update(3_400_000, at(0));
	for (let tick = 1; tick <= 100; tick++) {
		expect(r.update(tick % 2 === 0 ? 3_250_000 : 3_430_000, at(100 * tick))).toBeUndefined();
	}
	expect(r.target).toEqual({ enabled: true, bitrate: 3_400_000 });

	expect(r.update(3_000_000, at(20_000))).toEqual({ enabled: false });
	for (let tick = 1; tick <= 100; tick++) {
		expect(r.update(tick % 2 === 0 ? 3_250_000 : 3_430_000, at(20_000 + 100 * tick))).toBeUndefined();
	}
	expect(r.target).toEqual({ enabled: false });
});

test("a rung near its lower still resumes", () => {
	const r = new Rung(policy(1_000_000), 950_000);
	r.update(500_000, at(0));
	expect(r.target).toEqual({ enabled: false });
	expect(r.update(10_000_000, at(1000))?.enabled).toBe(true);
});

test("a rung without a band is refused", () => {
	expect(() => new Rung(policy(1_000_000), 1_000_000)).toThrow("must be below");
});
