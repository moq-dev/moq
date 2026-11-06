import { describe, expect, test } from "bun:test";
import { parseCamera } from "./paths.ts";

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
			"viewer/carla/truck1/abc",
		]) {
			expect(parseCamera(path)).toBeUndefined();
		}
	});
});
