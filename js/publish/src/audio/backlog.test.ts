import { expect, test } from "bun:test";
import { Micro } from "@moq/clock";
import { Backlog } from "./backlog";

// Repeated or regressing source timestamps must not turn a time budget into unlimited PCM.
test("sample count bounds raw retention even when capture time stands still", () => {
	const backlog = new Backlog(Micro(1_000_000), 48_000);
	for (let index = 0; index < 200; index++) {
		backlog.push({ timestamp: Micro(0), channels: [new Float32Array(960).fill(index)] }, Micro(20_000), Micro(0));
	}
	const retained = [];
	for (let frame = backlog.shift(); frame; frame = backlog.shift()) retained.push(frame.channels[0][0]);
	expect(retained).toEqual(Array.from({ length: 50 }, (_, index) => 150 + index));
});

test("sparse raw audio expires on the clock without more capture", () => {
	const backlog = new Backlog(Micro(1_000_000), 48_000);
	backlog.push({ timestamp: Micro(0), channels: [new Float32Array(960)] }, Micro(20_000), Micro(0));
	expect(backlog.deadline).toBe(Micro(1_020_000));
	backlog.expire(Micro(1_020_000));
	expect(backlog.shift()).toBeUndefined();
	expect(backlog.deadline).toBeUndefined();
});
