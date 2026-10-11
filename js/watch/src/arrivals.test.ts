import { expect, test } from "bun:test";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { type Arrival, recordArrival } from "./arrivals";

const frame = (arrival: number): Arrival => ({
	timestamp: Time.Milli(arrival),
	arrival: Time.Milli(arrival),
	late: false,
	skipped: false,
});

test("coalesced arrivals remain in immutable bounded snapshots", () => {
	const window = new Signal<readonly Arrival[]>([]);
	recordArrival(window, frame(0));
	const before = window.peek();
	for (let i = 1; i < 4100; i++) recordArrival(window, frame(i));
	expect(before).toEqual([frame(0)]);
	expect(window.peek()).toHaveLength(4096);
	expect(window.peek()[0].arrival).toBe(Time.Milli(4));
});

test("time eviction uses newest ingress even when delivery is reordered", () => {
	const window = new Signal<readonly Arrival[]>([]);
	recordArrival(window, frame(1000));
	recordArrival(window, frame(32000));
	recordArrival(window, frame(3000));
	recordArrival(window, frame(0));
	expect(window.peek()).toEqual([frame(32000), frame(3000)]);
});
