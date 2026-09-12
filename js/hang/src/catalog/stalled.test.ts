import { expect, test } from "bun:test";
import { Time } from "@moq/net";
import { CLEAR_FRAMES, DEFAULT_INTERVAL, Detector, intervalFromFps, SET_INTERVALS } from "./stalled";

const FRAME = Time.Micro.fromMilli(33 as Time.Milli);

function sample(lag: Time.Micro): Parameters<Detector["observe"]>[0] {
	return {
		frame: true,
		mediaLag: lag,
		quiet: 0 as Time.Micro,
		interval: FRAME,
		demand: true,
		idle: false,
	};
}

test("idle is never stalled", () => {
	const stalled = new Detector();
	expect(
		stalled.observe({
			frame: true,
			idle: true,
			demand: true,
			mediaLag: Time.Micro.fromSecond(10 as Time.Second),
			quiet: Time.Micro.fromSecond(10 as Time.Second),
			interval: FRAME,
		}),
	).toBe(false);
	expect(stalled.stalled).toBe(false);
});

test("no demand is never stalled", () => {
	const stalled = new Detector();
	expect(
		stalled.observe({
			frame: true,
			demand: false,
			idle: false,
			mediaLag: Time.Micro.fromSecond(10 as Time.Second),
			quiet: Time.Micro.fromSecond(10 as Time.Second),
			interval: FRAME,
		}),
	).toBe(false);
	expect(stalled.stalled).toBe(false);
});

test("lag past the threshold sets the flag", () => {
	const stalled = new Detector();
	expect(stalled.observe(sample((FRAME * SET_INTERVALS) as Time.Micro))).toBe(false);
	expect(stalled.observe(sample((FRAME * SET_INTERVALS + 1) as Time.Micro))).toBe(true);
	expect(stalled.flag()).toBe(true);
});

test("a quiet source sets the flag", () => {
	const stalled = new Detector();
	expect(
		stalled.observe({
			frame: true,
			mediaLag: 0 as Time.Micro,
			quiet: (FRAME * SET_INTERVALS + 1) as Time.Micro,
			interval: FRAME,
			demand: true,
			idle: false,
		}),
	).toBe(true);
	expect(stalled.stalled).toBe(true);
});

test("clearing needs a run of on-time frames", () => {
	const stalled = new Detector();
	stalled.observe(sample((FRAME * 10) as Time.Micro));
	for (let i = 0; i < CLEAR_FRAMES - 1; i++) {
		expect(stalled.observe(sample(10 as Time.Micro))).toBe(false);
		expect(stalled.stalled).toBe(true);
	}
	expect(stalled.observe(sample(10 as Time.Micro))).toBe(true);
	expect(stalled.stalled).toBe(false);
	expect(stalled.flag()).toBeUndefined();
});

test("dropping demand clears immediately", () => {
	const stalled = new Detector();
	stalled.observe(sample((FRAME * 10) as Time.Micro));
	expect(stalled.observe({ ...sample((FRAME * 10) as Time.Micro), demand: false })).toBe(true);
	expect(stalled.stalled).toBe(false);
});

test("a zero interval uses the default", () => {
	const stalled = new Detector();
	expect(
		stalled.observe({
			frame: true,
			mediaLag: (DEFAULT_INTERVAL * SET_INTERVALS + 1) as Time.Micro,
			quiet: 0 as Time.Micro,
			interval: 0 as Time.Micro,
			demand: true,
			idle: false,
		}),
	).toBe(true);
});

test("intervalFromFps falls back", () => {
	expect(intervalFromFps(undefined)).toBe(DEFAULT_INTERVAL);
	expect(intervalFromFps(0)).toBe(DEFAULT_INTERVAL);
	expect(intervalFromFps(Number.NaN)).toBe(DEFAULT_INTERVAL);
	expect(intervalFromFps(50)).toBe(Time.Micro.fromMilli(20 as Time.Milli));
});

test("polls do not count as recovery frames", () => {
	const stalled = new Detector();
	stalled.observe(sample((FRAME * 10) as Time.Micro));
	for (let i = 0; i < 10; i++) {
		expect(stalled.observe({ ...sample(0 as Time.Micro), frame: false })).toBe(false);
	}
	expect(stalled.stalled).toBe(true);
});

test("unrepresentable frame intervals use the default", () => {
	expect(intervalFromFps(Number.MIN_VALUE)).toBe(DEFAULT_INTERVAL);
	expect(intervalFromFps(Number.MAX_VALUE)).toBe(DEFAULT_INTERVAL);
});
