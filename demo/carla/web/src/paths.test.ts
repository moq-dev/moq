import { describe, expect, test } from "bun:test";
import { driverId, handle, parseCamera } from "./paths.ts";

describe("parseCamera", () => {
	test("accepts exactly carla/<truck>/<camera>.hang", () => {
		expect(parseCamera("carla/truck1/front.hang")).toEqual({ truck: "truck1", id: "truck1/front" });
	});

	test("ignores other broadcasts that share the relay", () => {
		for (const path of [
			"truck1/front.hang",
			"carla/front.hang",
			"carla/truck1/front",
			"carla/truck1/rear/extra.hang",
			"other/truck1/front.hang",
			"carla/.server/truck1",
			"carla/.client/driver",
		]) {
			expect(parseCamera(path)).toBeUndefined();
		}
	});
});

describe("driver names", () => {
	test("an endpoint id carries 128 random bits", () => {
		expect(driverId()).toMatch(/^driver-[0-9a-f]{32}$/);
		expect(driverId()).not.toBe(driverId());
	});

	test("a session's handle is the one the worker publishes", async () => {
		// Pinned by the worker's test_status_names_sessions_by_a_handle_the_player_derives_too.
		expect(await handle("0123456789abcdef0123456789abcdef")).toBe("driver-3eb1bd43");
	});
});
