import { describe, expect, test } from "bun:test";
import {
	expandToAspect,
	fitPoints,
	fleetTown,
	mapView,
	needsRefit,
	pathData,
	project,
	type RoadMap,
	scaleBar,
	speedLabel,
	type View,
	viewBox,
} from "./map.ts";

const square: View = { minX: -100, minY: -100, maxX: 100, maxY: 100 };

describe("expandToAspect", () => {
	test("widens a square view for a landscape box", () => {
		expect(expandToAspect(square, 2)).toEqual({ minX: -200, minY: -100, maxX: 200, maxY: 100 });
	});

	test("heightens a square view for a portrait box", () => {
		expect(expandToAspect(square, 0.5)).toEqual({ minX: -100, minY: -200, maxX: 100, maxY: 200 });
	});

	test("only ever grows, so nothing already in view is cropped out", () => {
		const wide: View = { minX: 0, minY: 0, maxX: 400, maxY: 100 };
		expect(expandToAspect(wide, 2)).toEqual({ minX: 0, minY: -50, maxX: 400, maxY: 150 });
	});

	test("leaves the view alone before the map box has been laid out", () => {
		expect(expandToAspect(square, Number.NaN)).toEqual(square);
		expect(expandToAspect(square, 0)).toEqual(square);
	});
});

describe("project", () => {
	test("puts the view's corners on the box's corners", () => {
		expect(project(square, { x: -100, y: -100 })).toEqual({ left: 0, top: 0 });
		expect(project(square, { x: 100, y: 100 })).toEqual({ left: 100, top: 100 });
	});

	test("maps CARLA's southward y onto a downward screen without a flip", () => {
		expect(project(square, { x: 0, y: 50 })).toEqual({ left: 50, top: 75 });
	});
});

describe("fitPoints", () => {
	test("has nothing to fit before a truck reports a position", () => {
		expect(fitPoints([])).toBeUndefined();
	});

	test("floors the span so a nose-to-tail convoy is not a wall of truck", () => {
		const view = fitPoints([
			{ x: 0, y: 0 },
			{ x: 12, y: 0 },
		]);
		expect(view).toBeDefined();
		expect((view as View).maxX - (view as View).minX).toBeCloseTo(300);
		expect((view as View).maxY - (view as View).minY).toBeCloseTo(300);
	});

	test("centres and pads a fleet spread wider than the floor", () => {
		const view = fitPoints([
			{ x: -500, y: 0 },
			{ x: 500, y: 0 },
		]) as View;
		expect(view.minX + view.maxX).toBeCloseTo(0);
		expect(view.maxX - view.minX).toBeCloseTo(1200);
	});
});

describe("needsRefit", () => {
	test("fits once there is something to fit", () => {
		expect(needsRefit(undefined, [])).toBe(false);
		expect(needsRefit(undefined, [{ x: 0, y: 0 }])).toBe(true);
	});

	test("holds the view still while the trucks drive around inside it", () => {
		expect(needsRefit(square, [{ x: 40, y: -40 }])).toBe(false);
	});

	test("refits when a truck reaches the edge", () => {
		expect(needsRefit(square, [{ x: 95, y: 0 }])).toBe(true);
		expect(needsRefit(square, [{ x: 0, y: -300 }])).toBe(true);
	});
});

describe("scaleBar", () => {
	test("picks a round length that fits inside a quarter of the view", () => {
		expect(scaleBar({ minX: 0, minY: 0, maxX: 1000, maxY: 1000 }).metres).toBe(200);
		expect(scaleBar({ minX: 0, minY: 0, maxX: 300, maxY: 300 }).metres).toBe(50);
	});

	test("reports the fraction of the view the bar covers", () => {
		expect(scaleBar({ minX: 0, minY: 0, maxX: 1000, maxY: 1000 }).fraction).toBeCloseTo(0.2);
	});
});

describe("baked road maps", () => {
	const map: RoadMap = {
		name: "Town10HD_Opt",
		bbox: [-10, -20, 30, 40],
		features: [
			{
				geometry: {
					coordinates: [
						[
							[0, 0],
							[10, 5],
						],
						[[1, 1]],
					],
				},
			},
		],
	};

	test("draws every lane as one path, skipping a lane with no length", () => {
		expect(pathData(map)).toBe("M0 0 L10 5");
	});

	test("takes the drawn view straight from the baked bounds", () => {
		expect(mapView(map)).toEqual({ minX: -10, minY: -20, maxX: 30, maxY: 40 });
		expect(viewBox(mapView(map))).toBe("-10 -20 40 60");
	});

	test("gives a town of one straight road something to draw on", () => {
		// A single east-west road bakes to a bbox with no height at all; drawn
		// as-is that is a zero SVG viewBox dimension and a non-finite scale bar.
		const road = mapView({ ...map, bbox: [0, 50, 400, 50] });
		expect(road).toEqual({ minX: 0, maxX: 400, minY: 45, maxY: 55 });
		expect(scaleBar(road).fraction).toBeCloseTo(0.25);

		const northSouth = mapView({ ...map, bbox: [50, 0, 50, 400] });
		expect(northSouth).toEqual({ minX: 45, maxX: 55, minY: 0, maxY: 400 });
		expect(Number.isFinite(scaleBar(northSouth).fraction)).toBe(true);
	});
});

describe("fleetTown", () => {
	test("has no town to show before any truck reports one", () => {
		expect(fleetTown([])).toBeUndefined();
	});

	test("shows the town most of the fleet is in", () => {
		expect(fleetTown(["Town10HD_Opt", "Town04", "Town10HD_Opt"])).toBe("Town10HD_Opt");
	});

	test("breaks a tie by name rather than by announcement order", () => {
		expect(fleetTown(["Town10HD_Opt", "Town04"])).toBe("Town04");
		expect(fleetTown(["Town04", "Town10HD_Opt"])).toBe("Town04");
	});
});

test("speed is labelled in km/h, which is how a truck reads", () => {
	expect(speedLabel(0)).toBe("0 km/h");
	expect(speedLabel(13.9)).toBe("50 km/h");
});

test("aerial imagery and roads fit together without shifting their coordinate origin", () => {
	const map: RoadMap = {
		name: "site",
		bbox: [0, 0, 100, 100],
		features: [],
		aerial: { image: "site.aerial.png", bbox: [-20, -10, 120, 110] },
	};
	const fitted = mapView(map);
	expect(fitted).toEqual({ minX: -20, minY: -10, maxX: 120, maxY: 110 });
	expect(project(fitted, { x: 50, y: 50 })).toEqual({ left: 50, top: 50 });
	map.aerial = { image: "site.aerial.png", bbox: [20, 20, 80, 80] };
	expect(mapView(map)).toEqual({ minX: 0, minY: 0, maxX: 100, maxY: 100 });
});

test("map padding leaves room for road shoulders without moving the world origin", () => {
	const map: RoadMap = { name: "site", bbox: [0, 0, 100, 100], features: [] };
	expect(mapView(map, 0.06)).toEqual({ minX: -6, minY: -6, maxX: 106, maxY: 106 });
	expect(project(mapView(map, 0.06), { x: 50, y: 50 })).toEqual({ left: 50, top: 50 });
});
