import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import { canvasPresentationTransform, rotateVideoDimensions } from "./presentation";

const output = { width: 640, height: 360 };
type Matrix = [number, number, number, number, number, number];
type Dimensions = { width: number; height: number };

const rotations: [number, Matrix, Dimensions][] = [
	[0, [1, 0, 0, 1, 0, 0], output],
	[90, [0, 1, -1, 0, 640, 0], { width: 360, height: 640 }],
	[180, [-1, 0, 0, -1, 640, 360], output],
	[270, [0, -1, 1, 0, 0, 360], { width: 360, height: 640 }],
];

const flips: [number, Matrix][] = [
	[0, [-1, 0, 0, 1, 640, 0]],
	[90, [0, 1, 1, 0, 0, 0]],
	[180, [1, 0, 0, -1, 0, 360]],
	[270, [0, -1, -1, 0, 640, 360]],
];

function video(rotation: number, flip = false): Catalog.Video {
	return { renditions: {}, rotation, flip };
}

describe("video presentation", () => {
	it.each(rotations)("rotates %i degrees clockwise", (rotation, matrix, source) => {
		expect(canvasPresentationTransform(output, video(rotation))).toEqual({ matrix, source });
		expect(rotateVideoDimensions(output, rotation)).toEqual(source);
	});

	it.each(flips)("flips the %i degree presentation horizontally", (rotation, matrix) => {
		expect(canvasPresentationTransform(output, video(rotation, true)).matrix).toEqual(matrix);
	});

	it("normalizes rotations to the nearest quarter-turn", () => {
		expect(canvasPresentationTransform(output, video(44)).matrix).toEqual([1, 0, 0, 1, 0, 0]);
		expect(canvasPresentationTransform(output, video(46)).matrix).toEqual([0, 1, -1, 0, 640, 0]);
		expect(canvasPresentationTransform(output, video(-90)).matrix).toEqual([0, -1, 1, 0, 0, 360]);
		expect(canvasPresentationTransform(output, video(450)).matrix).toEqual([0, 1, -1, 0, 640, 0]);
	});
});
