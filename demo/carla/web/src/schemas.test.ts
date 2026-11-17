import { describe, expect, test } from "bun:test";
import { TruckStatusSchema } from "./schemas.ts";

const status = {
	accepted: { steering: 0, throttle: 0, brake: 1, gear: "neutral" },
	estopped: false,
	media_pipeline: {},
};

describe("TruckStatusSchema", () => {
	test("defaults the optional fields", () => {
		const parsed = TruckStatusSchema.parse(status);
		expect(parsed.autonomy).toBe("unknown");
		expect(parsed.estop_by).toBeUndefined();
		expect(TruckStatusSchema.parse({ ...status, estopped: true, estop_by: "bystander" }).estop_by).toBe(
			"bystander",
		);
	});

	test("ignores fields it does not know", () => {
		const parsed = TruckStatusSchema.parse({ ...status, mode: "simulated", capabilities: ["respawn"] });
		expect(parsed).not.toHaveProperty("mode");
		expect(parsed.accepted.brake).toBe(1);
	});

	test("keeps an added autonomy value from rejecting all status", () => {
		const parsed = TruckStatusSchema.parse({ ...status, autonomy: "remote" });
		expect(parsed.autonomy).toBe("unknown");
		expect(parsed.accepted.brake).toBe(1);
	});
});
