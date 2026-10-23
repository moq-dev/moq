import { describe, expect, it } from "bun:test";
import { Order } from "./order";

const read = (group: number, timestamp: number) => ({ group, frame: { timestamp } });
const timestamps = (reads: ReturnType<typeof read>[]) => reads.map(({ frame }) => frame.timestamp);

describe("Order", () => {
	it("releases a held head sorted once a newer group arrives", () => {
		const order = new Order({ floor: 100 });
		for (const t of [40, 20, 0]) expect(order.push(read(t / 20, t))).toEqual([]);
		expect(timestamps(order.push(read(3, 60)))).toEqual([0, 20, 40, 60]);
		expect(timestamps(order.push(read(4, 80)))).toEqual([80]);
	});

	// A publisher may keep a long run of audio in one group, which never brings a newer one.
	it("releases a head once its first group spans the floor", () => {
		const order = new Order({ floor: 100 });
		expect(order.push(read(5, 0))).toEqual([]);
		expect(order.push(read(4, -20))).toEqual([]);
		expect(order.push(read(5, 80))).toEqual([]);
		expect(timestamps(order.push(read(5, 100)))).toEqual([-20, 0, 80, 100]);
	});

	it("releases the head as it stands on a flush", () => {
		const order = new Order({ floor: 100 });
		order.push(read(1, 20));
		order.push(read(0, 0));
		expect(timestamps(order.flush())).toEqual([0, 20]);
		expect(order.flush()).toEqual([]);
	});

	it("drops a frame older than one already released", () => {
		const order = new Order({ floor: 100 });
		order.push(read(1, 20));
		expect(timestamps(order.push(read(2, 40)))).toEqual([20, 40]);
		expect(order.push(read(0, 0))).toEqual([]);

		// A reset decoder restamps from its next input, so older frames are welcome again.
		order.reset();
		expect(timestamps(order.push(read(0, 0)))).toEqual([0]);
	});

	it("holds nothing live", () => {
		const order = new Order(undefined);
		expect(timestamps(order.push(read(1, 20)))).toEqual([20]);
		expect(order.push(read(0, 0))).toEqual([]);
	});
});
