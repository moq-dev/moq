import { describe, expect, it } from "bun:test";
import { Order } from "./order";

type Read = { group: number; frame: { timestamp: number } };
const read = (group: number, timestamp: number): Read => ({ group, frame: { timestamp } });
const timestamps = (reads: Read[]) => reads.map(({ frame }) => frame.timestamp);

describe("Order", () => {
	it("releases a held head sorted once a newer group arrives", () => {
		const order = new Order<Read>(() => ({ floor: 100, maxDelay: 1000 }));
		for (const t of [40, 20, 0]) expect(order.push(read(t / 20, t))).toEqual([]);
		expect(timestamps(order.push(read(3, 60)))).toEqual([0, 20, 40, 60]);
		expect(timestamps(order.push(read(4, 80)))).toEqual([80]);
	});

	// A publisher may keep a long run of audio in one group, which never brings a newer one.
	it("releases a head once its first group runs past the floor", () => {
		const order = new Order<Read>(() => ({ floor: 100, maxDelay: 1000 }));
		expect(order.push(read(5, 0))).toEqual([]);
		expect(order.push(read(4, -20))).toEqual([]);
		expect(order.push(read(5, 100))).toEqual([]);
		expect(timestamps(order.push(read(5, 120)))).toEqual([-20, 0, 100, 120]);
	});

	// With no floor, playback starts on the first frame, but the head still has to arrive first.
	it("holds the head past its first frame with a zero floor", () => {
		const order = new Order<Read>(() => ({ floor: 0, maxDelay: 1000 }));
		expect(order.push(read(1, 20))).toEqual([]);
		expect(order.push(read(0, 0))).toEqual([]);
		expect(timestamps(order.push(read(1, 40)))).toEqual([0, 20, 40]);
	});

	// Nothing older than the subscription's max delay is served, so a longer head is complete.
	it("releases a head once it spans the max delay", () => {
		const order = new Order<Read>(() => ({ floor: 100, maxDelay: 60 }));
		for (const t of [60, 40, 20]) expect(order.push(read(t / 20, t))).toEqual([]);
		expect(timestamps(order.push(read(0, 0)))).toEqual([0, 20, 40, 60]);
	});

	it("releases the head as it stands on a flush", () => {
		const order = new Order<Read>(() => ({ floor: 100, maxDelay: 1000 }));
		order.push(read(1, 20));
		order.push(read(0, 0));
		expect(timestamps(order.flush())).toEqual([0, 20]);
		expect(order.flush()).toEqual([]);
	});

	it("admits a frame only if it is no older than one already admitted", () => {
		const order = new Order<Read>(() => ({ floor: 100, maxDelay: 1000 }));
		order.push(read(1, 20));
		const released = order.push(read(2, 40));
		expect(released.every((r) => order.admit(r))).toBe(true);
		expect(order.push(read(0, 0))).toEqual([read(0, 0)]);
		expect(order.admit(read(0, 0))).toBe(false);

		// A reset decoder restamps from its next input, so older frames are welcome again.
		order.reset();
		expect(order.admit(read(0, 0))).toBe(true);
	});

	// A rise in the delay while the head arrives must not release it at the old floor.
	it("reads its bounds as the head arrives", () => {
		let floor = 20;
		const order = new Order<Read>(() => ({ floor, maxDelay: 1000 }));
		expect(order.push(read(5, 0))).toEqual([]);
		floor = 100;
		expect(order.push(read(5, 40))).toEqual([]);
		expect(order.push(read(4, -20))).toEqual([]);
		expect(timestamps(order.push(read(5, 120)))).toEqual([-20, 0, 40, 120]);
	});

	it("holds nothing live", () => {
		const order = new Order<Read>(undefined);
		expect(timestamps(order.push(read(1, 20)))).toEqual([20]);
		expect(order.admit(read(1, 20))).toBe(true);
		expect(timestamps(order.push(read(0, 0)))).toEqual([0]);
		expect(order.admit(read(0, 0))).toBe(false);
	});
});
