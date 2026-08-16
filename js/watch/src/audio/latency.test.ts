import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { reanchorFloor } from "./latency";

const ms = (value: number) => value as Time.Milli;

describe("reanchorFloor", () => {
	it("includes fixed latency and the largest media delay", () => {
		expect(reanchorFloor({ latency: ms(100), audio: ms(20), video: ms(80) })).toBe(ms(180));
	});

	it("tracks rendition delay without adaptive RTT jitter", () => {
		expect(reanchorFloor({ latency: "real-time", audio: ms(20), video: ms(80) })).toBe(ms(80));
		expect(reanchorFloor({ latency: "real-time", audio: ms(20), video: ms(200) })).toBe(ms(200));
	});
});
