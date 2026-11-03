import { describe, expect, test } from "bun:test";
import { type ControlHintState, controlHint } from "./hint.ts";

const active: ControlHintState = {
	active: true,
	focused: true,
	estopped: false,
	status: "pending",
	driverId: "mine",
	wheel: false,
};

describe("controlHint", () => {
	test("distinguishes an unavailable status track from a pending lease", () => {
		expect(controlHint(active)).toBe("Requesting control…");
		expect(controlHint({ ...active, status: "unavailable", statusDriver: "mine" })).toBe(
			"Vehicle status lost · Control unconfirmed",
		);
	});

	test("reports another lease holder once status is available", () => {
		expect(controlHint({ ...active, status: "available", statusDriver: "theirs" })).toBe("Driven by theirs");
	});

	test("reminds both input sources about the shift interlock", () => {
		const mine = { ...active, status: "available" as const, statusDriver: "mine" };
		expect(controlHint(mine)).toContain("Brake + G shifts");
		expect(controlHint({ ...mine, wheel: true })).toContain("Brake before shifting");
	});

	test("names the e-stop actor independently of the lease holder", () => {
		expect(controlHint({ ...active, estopped: true, estopBy: "bystander", statusDriver: "holder" })).toBe(
			"Stopped by bystander - R to reset",
		);
		expect(controlHint({ ...active, active: false, focused: false, estopped: true, estopBy: "bystander" })).toBe(
			"Stopped by bystander - Reset to resume",
		);
	});

	test("preserves preview and focus without requesting control", () => {
		expect(controlHint({ ...active, active: false, focused: false })).toBe("Open camera");
		expect(controlHint({ ...active, active: false })).toBe("Waiting for vehicle status");
		expect(controlHint({ ...active, active: false, status: "available", statusDriver: "theirs" })).toBe(
			"Driven by theirs",
		);
		expect(controlHint({ ...active, active: false, status: "unavailable" })).toBe(
			"Control unavailable · Esc to leave",
		);
	});
});
