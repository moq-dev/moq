import { expect, test } from "bun:test";
import { Slew } from "./slew.ts";

const TICK = 10_000_000;
const SETTLED = 2_000 + TICK / 2000;

// A host whose wall clock runs `ppm` fast of its monotonic one, read every tick.
class Host {
	slew = new Slew();
	at = 0;
	wall = 0;
	reading = this.slew.read(0, 0);

	// Let `micros` pass, checking each tick advances the timeline by a tick give or take the slew.
	// Returns how far the timeline reads ahead of the wall clock after it.
	run(micros: number, ppm = 0): number {
		for (let left = micros; left > 0; left -= TICK) {
			this.at += TICK;
			this.wall += TICK + (TICK * ppm) / 1e6;
			const before = this.reading;
			this.reading = this.slew.read(this.at, this.wall);
			expect(Math.abs(this.reading - before - TICK)).toBeLessThanOrEqual(TICK / 2000 + 1e-6);
		}
		return this.reading - this.wall;
	}
}

test("the timeline follows a wall clock drifting 500 ppm either way for a day", () => {
	const host = new Host();
	for (const ppm of [500, -500]) {
		expect(Math.abs(host.run(12 * 3600e6, ppm))).toBeLessThanOrEqual(SETTLED);
	}
});

test("a step back, or forward too small to be a suspend, is slewed out at 500 ppm", () => {
	for (const step of [-10e6, 1.5e6]) {
		const host = new Host();
		host.wall += step;
		// Half of it takes a thousand times its size.
		const half = Math.abs(step) * 1000;
		expect(Math.abs(host.run(half) + step / 2)).toBeLessThanOrEqual(2_000);
		expect(Math.abs(host.run(half + 100e6))).toBeLessThanOrEqual(SETTLED);
	}
});

test("a forward wall jump past two seconds is a suspend the timeline jumps", () => {
	const host = new Host();
	host.run(60e6);
	// A second out of step, being slewed, then suspended for an hour: the gap is jumped, and the
	// second is still slewed.
	host.wall += 1e6;
	const error = host.run(TICK);
	host.at += TICK;
	host.wall += TICK + 3600e6;
	const reading = host.slew.read(host.at, host.wall);
	expect(reading).toBe(host.reading + TICK + 3600e6 + TICK / 2000);
	expect(reading - host.wall).toBe(error + TICK / 2000);
});

test("another clock starts a new timeline on the wall clock", () => {
	const slew = new Slew();
	expect(slew.read(5e6, 7e6)).toBe(7e6);
	expect(slew.read(1e6, 1e6)).toBe(1e6);
});
