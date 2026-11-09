import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Backlog } from "./backlog";

// Repeated or regressing source timestamps must not turn a time budget into unlimited PCM.
test("sample count bounds raw retention even when capture time stands still", () => {
	const backlog = new Backlog(Duration.micros(1_000_000), 48_000);
	for (let index = 0; index < 200; index++) {
		backlog.push(
			{ timestamp: Duration.micros(0), channels: [new Float32Array(960).fill(index)] },
			Duration.micros(20_000),
			Duration.micros(0),
		);
	}
	const retained = [];
	for (let frame = backlog.shift(); frame; frame = backlog.shift()) retained.push(frame.channels[0][0]);
	expect(retained).toEqual(Array.from({ length: 50 }, (_, index) => 150 + index));
});

test("sparse raw audio expires on the clock without more capture", () => {
	const backlog = new Backlog(Duration.micros(1_000_000), 48_000);
	backlog.push(
		{ timestamp: Duration.micros(0), channels: [new Float32Array(960)] },
		Duration.micros(20_000),
		Duration.micros(0),
	);
	expect(backlog.deadline).toEqual(Duration.micros(1_020_000));
	backlog.expire(Duration.micros(1_020_000));
	expect(backlog.shift()).toBeUndefined();
	expect(backlog.deadline).toBeUndefined();
});

test("a break mark is reported once, on the frame that carries it", () => {
	const backlog = new Backlog(Duration.micros(1_000_000), 48_000);
	expect(backlog.empty).toBe(true);
	backlog.push(
		{ timestamp: Duration.micros(0), channels: [new Float32Array(960)] },
		Duration.micros(20_000),
		Duration.micros(0),
	);
	backlog.push(
		{ timestamp: Duration.micros(100_000), channels: [new Float32Array(960)] },
		Duration.micros(120_000),
		Duration.micros(0),
		true,
	);
	expect(backlog.empty).toBe(false);
	expect(backlog.takeCut()).toBe(false);
	backlog.shift();
	expect(backlog.takeCut()).toBe(true);
	expect(backlog.takeCut()).toBe(false);
	expect(backlog.shift()?.timestamp).toEqual(Duration.micros(100_000));
	expect(backlog.empty).toBe(true);
});
